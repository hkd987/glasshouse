// Chat's logic (CF-91 to CF-93, ADR 0030 C4a): what the page works out
// from what glassd gives it, with no DOM here, so it is tested on its own.
// The @-autocomplete, mention chips, the "New messages" divider, thread
// summaries, reaction pills, search hits and drafts' keys.

import { parseAttribution, showAttribution } from "@glasshouse/sdk";

export type Kind = "direct" | "group" | "channel";

/** How much of a conversation the person hears about (CF-92). */
export type Level = "all" | "mentions" | "none";

export interface Person {
  login: string;
  name: string;
}

/** A row of `chat.list`. The fields after `last_from` come with C4a. */
export interface Conversation {
  id: string;
  kind: Kind;
  title: string;
  people: Person[];
  /** Real messages not read yet, thread replies included. */
  unread: number;
  last_at: string | null;
  last_from: string | null;
  topic?: string | null;
  /** Unread messages that @mention the person. */
  mentions?: number;
  notify?: Level;
  /** The conversation's main draft, kept in glassd. */
  draft?: string | null;
}

/** `chat.read`'s `data.conversation`. */
export interface ConversationView {
  id: string;
  kind: Kind;
  title: string;
  topic?: string | null;
  /** Pinned messages' ids, most recently pinned first. */
  pinned?: string[];
  notify?: Level;
  draft?: string | null;
}

/** Something shared into the conversation (ADR 0033): an artifact shows as a card. */
export interface Shared {
  item: string;
  kind: string;
  title: string;
}

export interface Reaction {
  emoji: string;
  count: number;
  mine: boolean;
  /** Display names of who reacted. */
  names: string[];
}

export interface Message {
  id: string;
  seq: number;
  sender: string;
  sender_name: string;
  mine: boolean;
  at: string;
  text: string;
  attribution: string | null;
  reply_to?: string | null;
  shared?: Shared | null;
  edited?: boolean;
  deleted?: boolean;
  reactions?: Reaction[];
  pinned?: boolean;
  mentions?: Person[];
  /** Roots: how many replies, the last one's time, up to three repliers' names. */
  replies?: number;
  last_reply_at?: string | null;
  repliers?: string[];
  /** Replies: the root's id. */
  thread?: string | null;
}

/** One of `chat.search`'s hits. */
export interface Hit {
  conversation: string;
  title: string;
  message: string;
  /** The thread's root when the hit is a reply. */
  thread: string | null;
  sender_name: string;
  at: string;
  text: string;
}

export const TOPIC_MAX = 250;

/** How long typing rests before a draft is kept in glassd. */
export const DRAFT_DELAY_MS = 500;

/** The reactions offered first, and the grid behind "+". */
export const QUICK_EMOJI: readonly (readonly [string, string])[] = [
  ["👍", "thumbs up"],
  ["❤️", "heart"],
  ["😂", "laughing"],
  ["🎉", "party"],
  ["👀", "eyes"],
  ["🙏", "thank you"],
  ["✅", "done"],
  ["🔥", "fire"],
];

export const MORE_EMOJI: readonly (readonly [string, string])[] = [
  ["😀", "grinning"],
  ["😄", "smiling"],
  ["😊", "blushing"],
  ["🙂", "slight smile"],
  ["😉", "wink"],
  ["😍", "heart eyes"],
  ["🥳", "celebrating"],
  ["😎", "cool"],
  ["🤔", "thinking"],
  ["🤯", "mind blown"],
  ["😮", "surprised"],
  ["😅", "sweat smile"],
  ["😬", "grimacing"],
  ["😢", "sad"],
  ["😭", "crying"],
  ["🙃", "upside down"],
  ["😴", "sleepy"],
  ["🫡", "salute"],
  ["👏", "clapping"],
  ["🙌", "raised hands"],
  ["👌", "OK"],
  ["✌️", "victory"],
  ["💪", "strong"],
  ["👋", "wave"],
  ["🤝", "handshake"],
  ["👎", "thumbs down"],
  ["💯", "hundred"],
  ["✨", "sparkles"],
  ["⭐", "star"],
  ["🚀", "rocket"],
  ["💡", "idea"],
  ["📌", "pin"],
  ["📎", "paperclip"],
  ["📅", "calendar"],
  ["☕", "coffee"],
  ["🍕", "pizza"],
  ["❌", "no"],
  ["⚠️", "warning"],
  ["❓", "question"],
  ["➕", "plus one"],
];

/** The name an emoji is read out by, for labels. */
export function emojiName(emoji: string): string {
  return [...QUICK_EMOJI, ...MORE_EMOJI].find(([e]) => e === emoji)?.[1] ?? emoji;
}

export function initials(name: string): string {
  const parts = name.trim().split(/\s+/);
  return ((parts[0]?.[0] ?? "") + (parts.length > 1 ? (parts[parts.length - 1]?.[0] ?? "") : "")).toUpperCase() || "?";
}

function plural(n: number, one: string, many: string): string {
  return `${n} ${n === 1 ? one : many}`;
}

/** "and"-joined: "Priya", "Priya and Sam", "Priya, Sam and Lee". */
export function joinNames(names: readonly string[]): string {
  if (names.length <= 1) return names[0] ?? "";
  return `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;
}

function sameDay(a: Date, b: Date): boolean {
  return a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth() && a.getDate() === b.getDate();
}

/** How long ago `at` was, from `now`: "just now", "2m ago", "3h ago", "yesterday", "4d ago", "on Oct 3". */
export function ago(at: string, now: Date): string {
  const then = new Date(at);
  if (Number.isNaN(then.getTime())) return "";
  const minutes = Math.floor((now.getTime() - then.getTime()) / 60_000);
  if (minutes < 1) return "just now";
  if (minutes < 60) return `${minutes}m ago`;
  if (minutes < 60 * 24 && sameDay(then, now)) return `${Math.floor(minutes / 60)}h ago`;
  const yesterday = new Date(now);
  yesterday.setDate(now.getDate() - 1);
  if (sameDay(then, yesterday)) return "yesterday";
  if (minutes < 60 * 24 * 7) return `${Math.max(2, Math.round(minutes / (60 * 24)))}d ago`;
  return `on ${then.toLocaleDateString("en-US", { month: "short", day: "numeric" })}`;
}

/** The divider between days: "Today", "Yesterday", or the date. */
export function dayLabel(at: string, now: Date): string {
  const then = new Date(at);
  if (Number.isNaN(then.getTime())) return "";
  if (sameDay(then, now)) return "Today";
  const yesterday = new Date(now);
  yesterday.setDate(now.getDate() - 1);
  if (sameDay(then, yesterday)) return "Yesterday";
  return then.toLocaleDateString("en-US", {
    weekday: "long",
    month: "long",
    day: "numeric",
    ...(then.getFullYear() === now.getFullYear() ? {} : { year: "numeric" }),
  });
}

/** Whether two messages were sent on different days, where a day divider goes. */
export function newDay(previous: Message | undefined, m: Message): boolean {
  if (!previous) return true;
  const a = new Date(previous.at);
  const b = new Date(m.at);
  return Number.isNaN(a.getTime()) || Number.isNaN(b.getTime()) ? false : !sameDay(a, b);
}

/** Messages from the same person close together read as one block; an agent's never join. */
export function joins(previous: Message | undefined, m: Message): boolean {
  if (!previous || previous.sender !== m.sender || previous.deleted || newDay(previous, m)) return false;
  if (showAttribution(parseAttribution(m.attribution), "chat") || previous.attribution) return false;
  const gap = new Date(m.at).getTime() - new Date(previous.at).getTime();
  return Number.isNaN(gap) || gap < 5 * 60_000;
}

/** What shows under a thread's root in the timeline; null when it has no replies. */
export function threadSummary(root: Message, now: Date): { count: string; last: string; text: string; repliers: string[] } | null {
  const n = root.replies ?? 0;
  if (n <= 0) return null;
  const count = plural(n, "reply", "replies");
  const when = root.last_reply_at ? ago(root.last_reply_at, now) : "";
  const last = when ? `last reply ${when}` : "";
  return { count, last, text: last ? `${count} · ${last}` : count, repliers: (root.repliers ?? []).slice(0, 3) };
}

/** A reaction as its pill shows it: the count, and who on hover, "you" for the person. */
export interface Pill {
  emoji: string;
  count: number;
  mine: boolean;
  /** "Priya Shah and you reacted with 👍" */
  title: string;
  /** For a screen reader, and what clicking does. */
  label: string;
}

export function reactionPills(reactions: readonly Reaction[] | undefined, myName: string): Pill[] {
  return (reactions ?? [])
    .filter((r) => r.count > 0)
    .map((r) => {
      const others = r.names.filter((n) => !(r.mine && n === myName));
      const names = r.mine ? [...others, "you"] : others;
      const missing = r.count - names.length;
      const who = missing > 0 ? [...names, plural(missing, "other", "others")] : names;
      const said = joinNames(who);
      const title = said ? `${said.charAt(0).toUpperCase()}${said.slice(1)} reacted with ${r.emoji}` : `Reacted with ${r.emoji}`;
      const label = `${emojiName(r.emoji)}, ${plural(r.count, "reaction", "reactions")}${r.mine ? ", including yours. Remove yours" : ". Add yours"}`;
      return { emoji: r.emoji, count: r.count, mine: r.mine, title, label };
    });
}

/**
 * Where the "New messages" divider goes when a conversation opens: before
 * the earliest of the last `unread` messages others sent, worked out from
 * the unread count `chat.list` gave before `chat.read` marked it read.
 * The timeline holds thread roots only, so unread replies can put it a
 * little early; it never lands on the person's own message.
 */
export function firstUnread(messages: readonly Message[], unread: number): string | null {
  if (unread <= 0) return null;
  let left = unread;
  let first: string | null = null;
  for (let i = messages.length - 1; i >= 0 && left > 0; i--) {
    const m = messages[i];
    if (!m || m.mine) continue;
    first = m.id;
    left -= 1;
  }
  return first;
}

/** The person's last message they can edit (Up in an empty composer): not deleted, not a card, not an agent's. */
export function lastEditable(messages: readonly Message[]): Message | undefined {
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i];
    if (m && editable(m)) return m;
  }
  return undefined;
}

/** Whether the person may edit `m` here: theirs, there, text, and not written by an agent for them (AU-10). */
export function editable(m: Message): boolean {
  return m.mine && !m.deleted && !m.shared && !showAttribution(parseAttribution(m.attribution), "chat");
}

// --- Mentions ---------------------------------------------------------------------

const LOGIN_CHAR = /[A-Za-z0-9._-]/;

/**
 * The mention being typed at `caret`: from its "@" to the end of the login
 * typed so far. The "@" starts the text or follows a space or punctuation,
 * never a letter, so an email address isn't one.
 */
export function mentionQuery(text: string, caret: number): { start: number; end: number; query: string } | null {
  let start = caret;
  while (start > 0 && LOGIN_CHAR.test(text[start - 1] ?? "")) start--;
  if (text[start - 1] !== "@") return null;
  const at = start - 1;
  const before = text[at - 1];
  if (before !== undefined && (LOGIN_CHAR.test(before) || before === "@")) return null;
  let end = caret;
  while (end < text.length && LOGIN_CHAR.test(text[end] ?? "")) end++;
  return { start: at, end, query: text.slice(start, caret) };
}

/** The conversation's people for "@query": logins first, then names, never the person, at most `limit`. */
export function mentionMatches(people: readonly Person[], query: string, me: string, limit = 8): Person[] {
  const q = query.toLowerCase();
  const others = people.filter((p) => p.login !== me);
  const byLogin = others.filter((p) => p.login.toLowerCase().startsWith(q));
  const byName = others.filter((p) => !byLogin.includes(p) && p.name.toLowerCase().split(/\s+/).some((w) => w.startsWith(q)));
  return [...byLogin, ...byName].slice(0, limit);
}

/** `text` with the mention at [start, end) replaced by "@login ", and where the caret goes. */
export function insertMention(text: string, start: number, end: number, login: string): { text: string; caret: number } {
  const before = text.slice(0, start);
  const after = text.slice(end);
  const inserted = `@${login}${after.startsWith(" ") ? "" : " "}`;
  return { text: before + inserted + after, caret: before.length + inserted.length + (after.startsWith(" ") ? 1 : 0) };
}

export type Segment = { text: string } | { text: string; mention: Person };

function escape(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/**
 * A message's text cut where it @mentions someone glassd says it mentions,
 * so each can show as a chip. Longer logins are tried first, so "@sam"
 * never takes "@samantha".
 */
export function mentionSegments(text: string, mentions: readonly Person[] | undefined): Segment[] {
  const people = [...(mentions ?? [])].filter((p) => p.login).sort((a, b) => b.login.length - a.login.length);
  if (!people.length || !text) return text ? [{ text }] : [];
  const pattern = new RegExp(`(^|[^A-Za-z0-9._@-])@(${people.map((p) => escape(p.login)).join("|")})(?![A-Za-z0-9_-])(?!\\.[A-Za-z0-9])`, "g");
  const out: Segment[] = [];
  let last = 0;
  for (let m = pattern.exec(text); m; m = pattern.exec(text)) {
    const at = m.index + (m[1]?.length ?? 0);
    const login = m[2] ?? "";
    const person = people.find((p) => p.login === login);
    if (!person) continue;
    if (at > last) out.push({ text: text.slice(last, at) });
    out.push({ text: `@${login}`, mention: person });
    last = at + login.length + 1;
    pattern.lastIndex = last;
  }
  if (last < text.length) out.push({ text: text.slice(last) });
  return out;
}

/** Whether `m` mentions `me`. */
export function mentionsMe(m: Message, me: string): boolean {
  return !m.deleted && (m.mentions ?? []).some((p) => p.login === me);
}

// --- Search -----------------------------------------------------------------------

/** Hits grouped by conversation, in the order the first of each came. */
export function groupHits(hits: readonly Hit[]): { conversation: string; title: string; hits: Hit[] }[] {
  const groups: { conversation: string; title: string; hits: Hit[] }[] = [];
  for (const hit of hits) {
    let group = groups.find((g) => g.conversation === hit.conversation);
    if (!group) {
      group = { conversation: hit.conversation, title: hit.title, hits: [] };
      groups.push(group);
    }
    group.hits.push(hit);
  }
  return groups;
}

/** `text` cut where any word of `query` appears, ignoring case, so the matches can be marked. */
export function matchSegments(text: string, query: string): { text: string; match: boolean }[] {
  const words = [
    ...new Set(
      query
        .toLowerCase()
        .split(/\s+/)
        .filter((w) => w.length > 0),
    ),
  ];
  if (!words.length || !text) return text ? [{ text, match: false }] : [];
  const lower = text.toLowerCase();
  const marked = new Array<boolean>(text.length).fill(false);
  for (const word of words) {
    for (let i = lower.indexOf(word); i >= 0; i = lower.indexOf(word, i + word.length)) {
      for (let j = i; j < i + word.length; j++) marked[j] = true;
    }
  }
  const out: { text: string; match: boolean }[] = [];
  for (let i = 0; i < text.length; ) {
    let j = i;
    while (j < text.length && marked[j] === marked[i]) j++;
    out.push({ text: text.slice(i, j), match: marked[i] === true });
    i = j;
  }
  return out;
}

// --- Drafts and levels ------------------------------------------------------------

/** Where a draft lives: a conversation's main composer, or one thread's. */
export function draftKey(conversation: string, thread?: string | null): string {
  return thread ? `${conversation}/${thread}` : conversation;
}

export const LEVELS: readonly { level: Level; name: string; about: string }[] = [
  { level: "all", name: "All new messages", about: "Every message in this conversation" },
  { level: "mentions", name: "Mentions", about: "Only when someone @mentions you" },
  { level: "none", name: "Nothing", about: "Mute it: only an @mention still shows" },
];

/** What a sidebar row shows for unread messages, by the person's level for it. */
export function rowBadges(c: Conversation): { bold: boolean; count: number; mentions: number; muted: boolean } {
  const level = c.notify ?? "all";
  const mentions = c.mentions ?? 0;
  const muted = level === "none";
  return {
    bold: !muted && c.unread > 0,
    count: level === "all" && mentions === 0 ? c.unread : 0,
    mentions,
    muted,
  };
}
