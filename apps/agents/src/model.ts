// What the page says about agents, without a DOM: status words, the home
// screen's tabs and summary, how a conversation groups, answers and times
// as people say them. Unit tested (test/model.test.ts).

import { type AgentStatus, type AgentView, type ApprovalState, type Channel, type ComputerView, type Message, type PendingApproval, validId } from "./api.ts";

/** The app's pages, by path: the front serves the page at each. */
export type Route = { page: "home" } | { page: "new" } | { page: "agent"; id: string } | { page: "computer"; id: string } | { page: "missing" };

export function parseRoute(path: string): Route {
  const parts = path.split("/").filter(Boolean);
  if (parts.length === 0) return { page: "home" };
  if (parts[0] === "new" && parts.length === 1) return { page: "new" };
  let id = "";
  try {
    id = decodeURIComponent(parts[1] ?? "");
  } catch {
    return { page: "missing" };
  }
  if (parts[0] === "agents" && validId(id)) {
    if (parts.length === 2) return { page: "agent", id };
    if (parts.length === 3 && parts[2] === "computer") return { page: "computer", id };
  }
  return { page: "missing" };
}

export type Tone = "working" | "needs" | "yours" | "idle" | "asleep" | "starting";

/** An agent's status in a word or two, and the chip's color. */
export const STATUS: Record<AgentStatus, { label: string; tone: Tone }> = {
  starting: { label: "Starting", tone: "starting" },
  working: { label: "Working", tone: "working" },
  needs_you: { label: "Needs you", tone: "needs" },
  taken_over: { label: "You have it", tone: "yours" },
  idle: { label: "Idle", tone: "idle" },
  asleep: { label: "Asleep", tone: "asleep" },
};

export function statusWords(status: AgentStatus): { label: string; tone: Tone } {
  return STATUS[status] ?? { label: "Unknown", tone: "idle" };
}

/** The home screen's tabs. Idle shows only when an agent is idle. */
export type Tab = "all" | "needs" | "working" | "idle" | "asleep";

export function tabOf(status: AgentStatus): Exclude<Tab, "all"> {
  switch (status) {
    case "needs_you":
      return "needs";
    case "starting":
    case "working":
    case "taken_over":
      return "working";
    case "idle":
      return "idle";
    case "asleep":
      return "asleep";
  }
}

export function tabs(agents: AgentView[], current: Tab): { tab: Tab; label: string; count: number }[] {
  const count = (tab: Tab) => (tab === "all" ? agents.length : agents.filter((a) => tabOf(a.status) === tab).length);
  const all: [Tab, string][] = [
    ["all", "All"],
    ["needs", "Needs you"],
    ["working", "Working"],
    ["idle", "Idle"],
    ["asleep", "Asleep"],
  ];
  return all.filter(([tab]) => tab !== "idle" || count("idle") > 0 || current === "idle").map(([tab, label]) => ({ tab, label, count: count(tab) }));
}

export function visible(agents: AgentView[], tab: Tab): AgentView[] {
  return tab === "all" ? agents : agents.filter((a) => tabOf(a.status) === tab);
}

/** "3 agents: 1 working, 2 need you, 1 asleep". */
export function summary(agents: AgentView[]): string {
  if (agents.length === 0) return "No agents yet. Add one your IT team approved.";
  const n = (tab: Exclude<Tab, "all">) => agents.filter((a) => tabOf(a.status) === tab).length;
  const parts: string[] = [];
  if (n("working")) parts.push(`${n("working")} working`);
  if (n("needs")) parts.push(`${n("needs")} ${n("needs") === 1 ? "needs" : "need"} you`);
  if (n("idle")) parts.push(`${n("idle")} idle`);
  if (n("asleep")) parts.push(`${n("asleep")} asleep`);
  return `${plural(agents.length, "agent")}: ${parts.join(", ")}`;
}

/** The header's pill: how many agents wait on the person, by their
 * questions (an agent with two questions is one agent). */
export function waitingLabel(approvals: PendingApproval[]): string | null {
  const agents = new Set(approvals.map((p) => p.agent)).size;
  if (agents === 0) return null;
  return agents === 1 ? "1 needs you" : `${agents} need you`;
}

/** The card's last line: "Working now", "4 minutes ago". */
export function whenLine(agent: AgentView, now: Date): string {
  if (agent.status === "working") return "Working now";
  if (agent.status === "starting") return "Starting now";
  if (agent.status === "taken_over") return "You have your computer";
  return capital(ago(agent.updated_at, now));
}

/** Its computer, for people: a short word for the card's thumbnail, and
 * a sentence for beside the conversation. */
export function computerWords(computer: ComputerView): { short: string; line: string } {
  switch (computer.state) {
    case "new":
      return { short: "Not started", line: "Not started yet. It starts when you ask it something." };
    case "starting":
      return { short: "Starting", line: "Starting your computer. It's ready in a few seconds." };
    case "ready":
      return { short: "On", line: "On, with nothing running. Its own, kept between tasks." };
    case "running":
      return { short: "Running", line: "Running. Its own, kept between tasks." };
    case "saving":
      return { short: "Going to sleep", line: "Going to sleep: saving your computer to your company's storage." };
    case "asleep":
      return {
        short: "Asleep",
        line: "Asleep: saved to your company's storage, nothing running. Opening it wakes it where it left off.",
      };
    case "waking":
      return { short: "Waking", line: "Waking from storage. It picks up where it left off." };
  }
  return { short: "", line: "" };
}

/** Whether your computer can be put to sleep or woken now. */
export function sleepAction(computer: ComputerView): "sleep" | "wake" | null {
  if (computer.state === "asleep") return "wake";
  if (computer.state === "running" || computer.state === "ready") return "sleep";
  return null;
}

const VIA: Record<Channel, string> = {
  web: "",
  desktop: " on the desktop",
  cli: " from the command line",
  teams: " in Teams",
  api: " through another app",
};

/** What became of a question: "You approved at 10:58 in Teams." */
export function answerLine(state: ApprovalState, me: string, clock: (iso: string) => string): string | null {
  switch (state.state) {
    case "pending":
      return null;
    case "approved":
    case "denied": {
      const who = state.by === me ? "You" : state.by;
      const what = state.state === "approved" ? "approved" : "said no";
      const then = state.state === "denied" ? " Nothing was done." : "";
      return `${who} ${what}${VIA[state.via] ?? ""} at ${clock(state.at)}.${then}`;
    }
    case "expired":
      return "No answer in time. Nothing was done.";
  }
}

/** A conversation as the page draws it: day dividers, and runs of
 * messages from one side in one task, a few minutes apart, with one time
 * under each run. A task's progress steps in a row read as one card. */
export type Row = { kind: "day"; label: string } | { kind: "run"; from: Message["from"]; task: string; at: string; items: Message[] };

const RUN_GAP_MS = 5 * 60 * 1000;

export function groupMessages(messages: Message[], now: Date): Row[] {
  const rows: Row[] = [];
  let day = "";
  let run: Extract<Row, { kind: "run" }> | null = null;
  for (const message of messages) {
    const when = new Date(message.at);
    const label = dayLabel(when, now);
    if (label !== day) {
      rows.push({ kind: "day", label });
      day = label;
      run = null;
    }
    const joins = run && run.from === message.from && run.task === message.task && Date.parse(message.at) - Date.parse(run.at) <= RUN_GAP_MS;
    if (!run || !joins) {
      run = { kind: "run", from: message.from, task: message.task, at: message.at, items: [] };
      rows.push(run);
    }
    const last = run.items[run.items.length - 1];
    if (last && last.body.kind === "progress" && message.body.kind === "progress") {
      run.items[run.items.length - 1] = { ...last, at: message.at, body: { kind: "progress", steps: [...last.body.steps, ...message.body.steps] } };
    } else {
      run.items.push(message);
    }
    run.at = message.at;
  }
  return rows;
}

/** The task undo would put back (the newest that still can be), and when
 * it started. `POST …/undo` undoes the latest task. */
export function undoable(messages: Message[]): { task: string; message: string; since: string } | null {
  for (let i = messages.length - 1; i >= 0; i--) {
    const message = messages[i];
    if (message?.body.kind === "done" && message.body.undoable) {
      const first = messages.find((m) => m.task === message.task);
      return { task: message.task, message: message.id, since: first?.at ?? message.at };
    }
  }
  return null;
}

/** "Today", "Yesterday", "Monday", "12 October". */
export function dayLabel(when: Date, now: Date): string {
  const start = (d: Date) => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  const days = Math.round((start(now) - start(when)) / 86_400_000);
  if (days <= 0) return "Today";
  if (days === 1) return "Yesterday";
  if (days < 7) return WEEKDAYS[when.getDay()] ?? "";
  const date = `${when.getDate()} ${MONTHS[when.getMonth()] ?? ""}`;
  return when.getFullYear() === now.getFullYear() ? date : `${date} ${when.getFullYear()}`;
}

const WEEKDAYS = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const MONTHS = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

/** "just now", "4 minutes ago", "2 hours ago", "yesterday", "3 days ago". */
export function ago(iso: string, now: Date): string {
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return "";
  const minutes = Math.floor(Math.max(0, now.getTime() - then) / 60_000);
  if (minutes < 1) return "just now";
  if (minutes < 60) return `${plural(minutes, "minute")} ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${plural(hours, "hour")} ago`;
  const days = Math.floor(hours / 24);
  return days === 1 ? "yesterday" : `${days} days ago`;
}

/** "DA" for Dana Alvarez. */
export function initials(name: string): string {
  const words = name.trim().split(/\s+/).filter(Boolean);
  const letters = words.length > 1 ? [words[0], words[words.length - 1]] : words;
  return letters.map((w) => (w ? Array.from(w)[0] ?? "" : "")).join("").toUpperCase();
}

/** "2.4 GB". */
export function size(bytes: number): string {
  const units = ["bytes", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1000 && unit < units.length - 1) {
    value /= 1000;
    unit += 1;
  }
  return unit === 0 ? `${value} bytes` : `${value < 10 ? value.toFixed(1) : Math.round(value)} ${units[unit]}`;
}

/** A path's file name and the folder it's in: "march.xlsx", "Finance/March". */
export function splitPath(path: string): { name: string; folder: string } {
  const at = path.lastIndexOf("/");
  return at < 0 ? { name: path, folder: "" } : { name: path.slice(at + 1), folder: path.slice(0, at) };
}

/** What a new message says, for the screen reader's live region. */
export function spoken(agent: string, message: Message): string {
  const body = message.body;
  switch (body.kind) {
    case "text":
      return message.from === "person" ? "" : `${agent}: ${body.text}`;
    case "progress":
      return `${agent}: ${body.steps[body.steps.length - 1] ?? ""}`;
    case "approval":
      return body.confidential ? `${agent} needs you.` : `${agent} needs you: ${body.summary}`;
    case "done":
      return `${agent}: ${body.text}`;
    case "note":
      return `${agent}: ${body.text}`;
  }
}

export function plural(n: number, one: string, many = `${one}s`): string {
  return `${n} ${n === 1 ? one : many}`;
}

export function capital(text: string): string {
  return text.charAt(0).toUpperCase() + text.slice(1);
}
