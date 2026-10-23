// Routines' logic, without a DOM (ADR 0047): what glassd says about the
// person's routines, in the words the page shows, and the schedule picker's
// choices as the few words routines.schedule and routines.keep take.

/** One run of a routine, as glassd kept it. */
export interface Run {
  task: string;
  version: number;
  scheduled: boolean;
  started_at: string;
  ended_at: string;
  outcome: "done" | "failed" | "stopped";
  /** A model was asked: the script didn't finish it, and the agent did. */
  model: boolean;
  said: string;
  changes: number;
}

/** A routine as routines.list gives it. */
export interface Routine {
  routine: string;
  name: string;
  version: number;
  request: string;
  /** Its schedule in the words routines take ("fri 17:00"), or null. */
  schedule: string | null;
  /** The same in a sentence: "every Friday at 17:00", "when started". */
  runs: string;
  calls: string[];
  read: string[];
  write: string[];
  paused: boolean;
  next_run_at: string | null;
  last_run: Run | null;
  /** Its run going now, wherever it was started. */
  running: string | null;
  note: string;
  saved_at: string;
}

/** One saved version of a routine, as routines.read lists them. */
export interface Version {
  version: number;
  note: string;
  saved_at: string;
  /** When it runs, in a sentence. */
  runs: string;
  /** The one that runs. */
  in_use: boolean;
}

/** routines.read: the same, with its code, the runs kept and its versions, newest first. */
export interface RoutineDetail extends Routine {
  script: string;
  runs_kept: Run[];
  /** This is the version that runs. */
  in_use: boolean;
  versions: Version[];
}

/** A request done the same way more than once (ADR 0046). */
export interface Repeat {
  task: string;
  request: string;
  agent: string;
  times: number;
  last_at: string;
  script: string;
  steps: string[];
  read: string[];
  write: string[];
}

const DAYS = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"] as const;
export type Day = (typeof DAYS)[number];
const DAY_NAMES: Record<Day, string> = {
  mon: "Monday",
  tue: "Tuesday",
  wed: "Wednesday",
  thu: "Thursday",
  fri: "Friday",
  sat: "Saturday",
  sun: "Sunday",
};

/** What the schedule picker offers. `other` keeps a schedule it can't show, as it is. */
export type Choice =
  | { kind: "off" }
  | { kind: "daily" | "weekdays" | "weekends"; at: string }
  | { kind: "weekly"; day: Day; at: string }
  | { kind: "hourly" }
  | { kind: "other"; text: string };

export const DAY_CHOICES: readonly { day: Day; name: string }[] = DAYS.map((day) => ({ day, name: DAY_NAMES[day] }));

const TIME = /^([01]\d|2[0-3]):[0-5]\d$/;

/** A schedule's words as the picker shows them. */
export function choiceOf(schedule: string | null): Choice {
  if (!schedule) return { kind: "off" };
  if (schedule === "hourly") return { kind: "hourly" };
  const [days, at = ""] = schedule.split(" ");
  if (!TIME.test(at)) return { kind: "other", text: schedule };
  if (days === "daily" || days === "weekdays" || days === "weekends") return { kind: days, at };
  if ((DAYS as readonly string[]).includes(days ?? "")) return { kind: "weekly", day: days as Day, at };
  return { kind: "other", text: schedule };
}

/** The picker's choice in the words routines take; null runs it only when started. */
export function scheduleOf(choice: Choice): string | null {
  switch (choice.kind) {
    case "off":
      return null;
    case "hourly":
      return "hourly";
    case "other":
      return choice.text;
    case "weekly":
      return `${choice.day} ${choice.at}`;
    default:
      return `${choice.kind} ${choice.at}`;
  }
}

/** Whether a choice is one routines will take: a time of day in 24 hours. */
export function choiceProblem(choice: Choice): string | null {
  if ("at" in choice && !TIME.test(choice.at)) return "Pick a time of day.";
  return null;
}

/** When something happens next, as a person says it: "today at 17:00", "Friday at 17:00". */
export function upcoming(at: string, now: Date): string {
  const when = new Date(at);
  const days = dayDiff(now, when);
  const time = clock(when);
  if (days === 0) return `today at ${time}`;
  if (days === 1) return `tomorrow at ${time}`;
  if (days > 1 && days < 7) return `${weekday(when)} at ${time}`;
  return `${monthDay(when)} at ${time}`;
}

/** When something happened, as a person says it: "just now", "today at 09:00", "yesterday at 17:00". */
export function past(at: string, now: Date): string {
  const when = new Date(at);
  const seconds = (now.getTime() - when.getTime()) / 1000;
  if (seconds < 60) return "just now";
  if (seconds < 3600) {
    const minutes = Math.floor(seconds / 60);
    return minutes === 1 ? "a minute ago" : `${minutes} minutes ago`;
  }
  const days = dayDiff(when, now);
  const time = clock(when);
  if (days === 0) return `today at ${time}`;
  if (days === 1) return `yesterday at ${time}`;
  if (days < 7) return `${weekday(when)} at ${time}`;
  return `${monthDay(when)} at ${time}`;
}

/** Whole calendar days from `a` to `b`, in local time. */
function dayDiff(a: Date, b: Date): number {
  const start = (d: Date) => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  return Math.round((start(b) - start(a)) / 86_400_000);
}

function clock(d: Date): string {
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
}

function weekday(d: Date): string {
  return ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"][d.getDay()] ?? "";
}

function monthDay(d: Date): string {
  const month = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"][d.getMonth()];
  return `${month} ${d.getDate()}`;
}

/** When a routine runs, in a line: "Runs every Friday at 17:00 · next today at 17:00". */
export function whenLine(r: Routine, now: Date): string {
  if (r.paused) return r.schedule ? `Paused · it runs only when you start it` : "Runs when you start it";
  if (!r.schedule) return "Runs when you start it";
  const runs = `Runs ${r.runs}`;
  return r.next_run_at ? `${runs} · next ${upcoming(r.next_run_at, now)}` : runs;
}

export type Tone = "ok" | "stepped-in" | "bad";

/** How a run went, in a few words and a tone for its dot. */
export function outcome(run: Run): { text: string; tone: Tone } {
  if (run.outcome === "stopped") return { text: "Stopped", tone: "bad" };
  if (run.outcome === "failed") return { text: "Didn't finish", tone: "bad" };
  if (run.model) return { text: "The agent stepped in", tone: "stepped-in" };
  return { text: "Ran with no model", tone: "ok" };
}

/** Words a name doesn't end on: "… from Vendor News in" reads as cut off. */
const DANGLING = new Set(["a", "an", "and", "at", "by", "for", "from", "in", "into", "my", "of", "on", "or", "the", "to", "with"]);

/** A name for a request, to start from: its first words, as a title. */
export function nameFor(request: string): string {
  const words = request
    .replace(/[.!?]+$/, "")
    .split(/\s+/)
    .filter(Boolean);
  const kept: string[] = [];
  for (const word of words) {
    if ([...kept, word].join(" ").length > 40) break;
    kept.push(word);
  }
  while (kept.length > 1 && DANGLING.has((kept.at(-1) ?? "").toLowerCase())) kept.pop();
  const name = kept.join(" ") || request.trim().slice(0, 40);
  return name.charAt(0).toUpperCase() + name.slice(1);
}

/** A folder as the person knows it: in their home, from ~. */
export function folder(path: string, user: string): string {
  for (const home of [`/home/${user}`, `/usr/home/${user}`]) {
    if (path === home) return "~";
    if (path.startsWith(`${home}/`)) return `~/${path.slice(home.length + 1)}`;
  }
  return path;
}

/** A tool as the code calls it, in words: "mail_move" → "mail move". */
export function tool(name: string): string {
  return name.replaceAll("_", " ");
}

/** A line of one version's code against another's. */
export interface DiffLine {
  kind: "same" | "added" | "removed";
  text: string;
}

/**
 * `to`'s lines against `from`'s: those both have, in order, and those
 * only one has, the fewest of them (a longest common subsequence). Code
 * over 2,000 lines comes back whole, as removed then added.
 */
export function diffLines(from: string, to: string): DiffLine[] {
  const a = from.replace(/\n+$/, "").split("\n");
  const b = to.replace(/\n+$/, "").split("\n");
  if (a.length > 2000 || b.length > 2000) {
    return [...a.map((text) => ({ kind: "removed" as const, text })), ...b.map((text) => ({ kind: "added" as const, text }))];
  }
  // longest[i][j]: the longest run of lines a[i..] and b[j..] have in common.
  const longest = Array.from({ length: a.length + 1 }, () => new Uint16Array(b.length + 1));
  for (let i = a.length - 1; i >= 0; i -= 1) {
    for (let j = b.length - 1; j >= 0; j -= 1) {
      longest[i]![j] = a[i] === b[j] ? longest[i + 1]![j + 1]! + 1 : Math.max(longest[i + 1]![j]!, longest[i]![j + 1]!);
    }
  }
  const out: DiffLine[] = [];
  let i = 0;
  let j = 0;
  while (i < a.length && j < b.length) {
    if (a[i] === b[j]) {
      out.push({ kind: "same", text: a[i]! });
      i += 1;
      j += 1;
    } else if (longest[i + 1]![j]! >= longest[i]![j + 1]!) {
      out.push({ kind: "removed", text: a[i]! });
      i += 1;
    } else {
      out.push({ kind: "added", text: b[j]! });
      j += 1;
    }
  }
  for (; i < a.length; i += 1) out.push({ kind: "removed", text: a[i]! });
  for (; j < b.length; j += 1) out.push({ kind: "added", text: b[j]! });
  return out;
}
