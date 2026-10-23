// Mail's logic, apart from the page: what mail.list and mail.read return,
// how dates read, how a reply starts, which draft answers which message.

import { type Attribution, parseAddresses } from "@glasshouse/sdk";

export type Folder = "inbox" | "drafts" | "sent" | "archive" | "junk" | "trash";
export const FOLDERS: readonly { id: Folder; title: string }[] = [
  { id: "inbox", title: "Inbox" },
  { id: "drafts", title: "Drafts" },
  { id: "sent", title: "Sent" },
  { id: "archive", title: "Archive" },
  { id: "junk", title: "Junk" },
  { id: "trash", title: "Trash" },
];

/** Where mail.move files a message (ADR 0037). */
export type Destination = "inbox" | "archive" | "junk" | "trash";

/** The moves a message in `folder` offers, as buttons read: drafts stay put. */
export function moves(folder: Folder): { to: Destination; label: string }[] {
  switch (folder) {
    case "inbox":
      return [
        { to: "archive", label: "Archive" },
        { to: "junk", label: "Junk" },
        { to: "trash", label: "Delete" },
      ];
    case "archive":
      return [
        { to: "inbox", label: "Move to Inbox" },
        { to: "junk", label: "Junk" },
        { to: "trash", label: "Delete" },
      ];
    case "junk":
      return [
        { to: "inbox", label: "Not junk" },
        { to: "trash", label: "Delete" },
      ];
    case "trash":
      return [
        { to: "inbox", label: "Move to Inbox" },
        { to: "archive", label: "Archive" },
      ];
    case "sent":
      return [
        { to: "archive", label: "Archive" },
        { to: "trash", label: "Delete" },
      ];
    case "drafts":
      return [];
  }
}

/**
 * Where a message's HTML is shown: its own origin, `view.mail.localhost`
 * on the bridge's port, under a CSP with no scripts and no network (ADR
 * 0037). Null for an id that isn't a plain `<folder>/<file>.eml`.
 */
export function viewUrl(location: { protocol: string; port: string }, id: string): string | null {
  if (!/^[a-z]+\/[A-Za-z0-9_-][A-Za-z0-9._-]*\.eml$/.test(id) || id.length > 210) return null;
  const port = location.port ? `:${location.port}` : "";
  return `${location.protocol}//view.mail.localhost${port}/${id}`;
}

/** One message as mail.list returns it. */
export interface Summary {
  id: string;
  folder: Folder;
  from: string;
  to: string[];
  subject: string;
  date: string;
  snippet: string;
  /** Who wrote it, when an agent or app did (AU-10). */
  attribution: Attribution | null;
  /** Not opened by the person yet (ADR 0050); drafts and sent mail never are. */
  unread: boolean;
}

/** Whether a folder keeps read state: drafts and sent mail are the person's own. */
export function tracksReading(folder: Folder): boolean {
  return folder !== "drafts" && folder !== "sent";
}

/** A file a message carries, by name: Mail never has its contents. */
export interface AttachmentInfo {
  name: string;
  size: number;
  type: string;
}

/** One message as mail.read returns it. */
export interface Message extends Summary {
  body: string;
  /** Its HTML, handed to Mail only; shown framed, never as markup here. */
  html?: string | null;
  attachments?: AttachmentInfo[];
}

/** What mail.draft and mail.send take. */
export interface Outgoing {
  to: string[];
  subject: string;
  body: string;
  /** The draft it is: the files that draft carries go with it (ADR 0048). */
  draft?: string;
}

/** A file's size as people read it: "240 KB". */
export function fileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} bytes`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/** "Dana Ruiz <dana@northwind.example>" → "Dana Ruiz". */
export function displayName(from: string): string {
  const match = /^\s*"?([^"<]*?)"?\s*<[^>]*>\s*$/.exec(from);
  const name = match?.[1]?.trim();
  return name || address(from) || from;
}

/** "Dana Ruiz <dana@northwind.example>" → "dana@northwind.example". */
export function address(from: string): string {
  const match = /<([^>]+)>/.exec(from);
  return (match?.[1] ?? from).trim();
}

/** Initials for the avatar: "Dana Ruiz" → "DR". */
export function initials(from: string): string {
  const words = displayName(from)
    .replace(/@.*/, "")
    .split(/[\s._-]+/)
    .filter(Boolean);
  if (words.length === 1) return (words[0] ?? "").slice(0, 2).toUpperCase();
  return words
    .slice(0, 2)
    .map((w) => w[0]?.toUpperCase() ?? "")
    .join("");
}

const TIME = new Intl.DateTimeFormat("en-US", { hour: "numeric", minute: "2-digit" });
const WEEKDAY = new Intl.DateTimeFormat("en-US", { weekday: "short" });
const DAY = new Intl.DateTimeFormat("en-US", { month: "short", day: "numeric" });
const FULL = new Intl.DateTimeFormat("en-US", {
  weekday: "short",
  month: "short",
  day: "numeric",
  hour: "numeric",
  minute: "2-digit",
});

/** A list's date: the time today, the weekday this week, else the day. */
export function listDate(date: string, now: Date = new Date()): string {
  const when = new Date(date);
  if (Number.isNaN(when.getTime())) return "";
  const startOfToday = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  if (when >= startOfToday) return TIME.format(when);
  const days = (startOfToday.getTime() - when.getTime()) / 86_400_000;
  if (days < 6) return WEEKDAY.format(when);
  return DAY.format(when);
}

/** The reading pane's date: "Mon, Oct 19, 9:42 AM". */
export function fullDate(date: string): string {
  const when = new Date(date);
  return Number.isNaN(when.getTime()) ? date : FULL.format(when);
}

function bareSubject(subject: string): string {
  return subject.replace(/^\s*((re|fwd?)\s*:\s*)+/i, "").trim().toLowerCase();
}

/** The reply a person starts from: to the sender, "Re:", the message quoted. */
export function replyTo(message: Message): Outgoing {
  const subject = /^\s*re\s*:/i.test(message.subject) ? message.subject : `Re: ${message.subject}`;
  const quoted = message.body
    .trimEnd()
    .split("\n")
    .map((line) => (line ? `> ${line}` : ">"))
    .join("\n");
  return {
    to: [address(message.from)],
    subject,
    body: `\n\nOn ${fullDate(message.date)}, ${displayName(message.from)} wrote:\n${quoted}\n`,
  };
}

/** The draft that answers `message`, if one does: "Re:" its subject, to its sender. */
export function draftFor(message: Summary, drafts: readonly Summary[]): Summary | null {
  const sender = address(message.from).toLowerCase();
  const subject = bareSubject(message.subject);
  return (
    drafts.find(
      (d) =>
        /^\s*re\s*:/i.test(d.subject) &&
        bareSubject(d.subject) === subject &&
        d.to.some((to) => address(to).toLowerCase() === sender),
    ) ?? null
  );
}

/** Whether `draft` has been sent since: the same subject and people, later. */
export function wasSent(draft: Summary, sent: readonly Summary[]): boolean {
  const people = (s: Summary) =>
    s.to
      .map((t) => address(t).toLowerCase())
      .sort()
      .join(",");
  return sent.some(
    (s) =>
      s.subject === draft.subject &&
      people(s) === people(draft) &&
      new Date(s.date).getTime() >= new Date(draft.date).getTime(),
  );
}

/** Splits what someone typed in To: into addresses, and what isn't one. */
export function parseRecipients(text: string): { to: string[]; bad: string[] } {
  const { addresses, bad } = parseAddresses(text);
  return { to: addresses, bad };
}

/** What a compose form lacks before it can be sent or saved. */
export function problems(message: Outgoing, bad: readonly string[]): string[] {
  const found: string[] = [];
  if (bad.length) found.push(`Not an address: ${bad.join(", ")}`);
  if (message.to.length === 0) found.push("Add someone to send it to.");
  if (!message.subject.trim()) found.push("Add a subject.");
  return found;
}
