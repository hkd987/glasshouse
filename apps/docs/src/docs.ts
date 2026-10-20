// Docs' logic (DC-1, DC-3, DC-4, ADR 0035), kept apart from the page so the
// unit tests cover it without a DOM: ids and links, who may do what, what
// the list shows, and what create, roles, comments and versions send. The
// live session is live.ts; the document's schema is schema.ts.

export type Role = "owner" | "editor" | "commenter" | "viewer";
/** The roles someone else can have in a doc: the owner is whoever made it. */
export type Share = "editor" | "commenter" | "viewer";

/** What a doc's data is labeled (DP): who may see it, and what may read it. */
export type Label = "public" | "internal" | "confidential" | "restricted";

/** The labels a person may make a doc at from this app (its manifest's), most open first. */
export const DOC_LABELS: readonly Label[] = ["public", "internal", "confidential"];

export interface Person {
  login: string;
  name: string;
  role?: Role;
}

/** One row of `doc.list`. */
export interface Row {
  id: string;
  title: string;
  label?: Label;
  owner: string;
  owner_name: string;
  mine: boolean;
  role: Role;
  people: Person[];
  updated_at: string | null;
}

/** A named version (DC-3). */
export interface Version {
  id: string;
  name: string;
  by_name: string;
  at: string;
}

/** Where a comment is: a Yjs relative position in the document, base64. */
export interface Anchor {
  path?: string | null;
}

export interface Comment {
  id: string;
  by: string;
  by_name: string;
  attribution: string | null;
  text: string;
  anchor: Anchor | null;
  reply_to: string | null;
  resolved: boolean;
  at: string;
}

/** `doc.open`: the doc, its whole state as one Yjs update, and where its log is. */
export interface Opened {
  id: string;
  title: string;
  label?: Label;
  owner: string;
  owner_name: string;
  role: Role;
  people: Person[];
  state: string;
  cursor: number;
  versions: Version[];
  comments: Comment[];
}

/** Someone `people.search` found. */
export interface Found {
  login: string;
  name: string;
  email?: string;
  title?: string;
}

/** A doc's id (its conversation's): `cv-` and 32 hex digits. Artifacts' are the same shape. */
export const ID = /^cv-[0-9a-f]{32}$/;

/** A comment's longest text (chat's MAX_TEXT_CHARS). */
export const MAX_TEXT_CHARS = 8000;
/** A title's longest (MAX_TITLE_CHARS). */
export const MAX_TITLE_CHARS = 200;
/** A version name's longest. */
export const MAX_VERSION_CHARS = 120;
/** The longest link kept. */
export const MAX_HREF = 2048;

export function isDocId(value: unknown): value is string {
  return typeof value === "string" && ID.test(value);
}

export const isArtifactId = isDocId;

/** The doc a location's hash names (`#cv-<hex>`), if any. */
export function deepLink(hash: string): string | null {
  const id = hash.replace(/^#/, "");
  return isDocId(id) ? id : null;
}

/** What was shared with the person, and their own, each newest first. */
export function partition(rows: readonly Row[]): { shared: Row[]; mine: Row[] } {
  const newest = (a: Row, b: Row) => (b.updated_at ?? "").localeCompare(a.updated_at ?? "") || a.title.localeCompare(b.title);
  return {
    shared: rows.filter((r) => !r.mine).sort(newest),
    mine: rows.filter((r) => r.mine).sort(newest),
  };
}

/** A label as people read it. */
export function labelName(label: unknown): string | null {
  switch (label) {
    case "public":
      return "Public";
    case "internal":
      return "Internal";
    case "confidential":
      return "Confidential";
    case "restricted":
      return "Restricted";
    default:
      return null;
  }
}

/** A role as people read it. */
export function roleName(role: unknown): string {
  switch (role) {
    case "owner":
      return "Owner";
    case "editor":
      return "Can edit";
    case "commenter":
      return "Can comment";
    default:
      return "Can view";
  }
}

export function canEdit(role: Role | null | undefined): boolean {
  return role === "owner" || role === "editor";
}

/** Commenters comment on a doc they can't edit; viewers only read it (ADR 0035 §2). */
export function canComment(role: Role | null | undefined): boolean {
  return role === "owner" || role === "editor" || role === "commenter";
}

/** Never the base system's name on screen (BR-1), whatever a result says. */
export function readable(text: string): string {
  return text.replace(/free\s*bsd/gi, "the system");
}

/** "just now", "5 min ago", "3 h ago", then the date. */
export function when(at: string | null | undefined, now: Date = new Date()): string {
  if (!at) return "";
  const d = new Date(at);
  if (Number.isNaN(d.getTime())) return "";
  const minutes = Math.floor((now.getTime() - d.getTime()) / 60_000);
  if (minutes < 1) return "just now";
  if (minutes < 60) return `${minutes} min ago`;
  if (minutes < 24 * 60) return `${Math.floor(minutes / 60)} h ago`;
  return d.toLocaleDateString("en-US", { month: "short", day: "numeric" });
}

/** "Priya Shah", "Priya Shah and Sam Okafor", "Priya Shah, Sam Okafor and 2 more". */
export function names(list: readonly string[], most = 2): string {
  if (list.length <= most + 1) return list.length > 1 ? `${list.slice(0, -1).join(", ")} and ${list.at(-1)}` : (list[0] ?? "");
  return `${list.slice(0, most).join(", ")} and ${list.length - most} more`;
}

export function initials(name: string): string {
  const parts = name.trim().split(/\s+/);
  return ((parts[0]?.[0] ?? "") + (parts.length > 1 ? (parts[parts.length - 1]?.[0] ?? "") : "")).toUpperCase() || "?";
}

// --- Links ---------------------------------------------------------

const CONTROL = /[\u0000- \u007f]/;

/**
 * A link the page may open: another doc (`#cv-<hex>`), the web (http,
 * https) or mail (mailto). Anything else (`javascript:`, `data:`, a path
 * on this origin) is null, whoever wrote it.
 */
export function safeHref(value: unknown): string | null {
  if (typeof value !== "string") return null;
  const href = value.trim();
  if (!href || href.length > MAX_HREF || CONTROL.test(href)) return null;
  if (href.startsWith("#")) return deepLink(href) ? href : null;
  if (/^mailto:[^\s@]+@[^\s@]+$/i.test(href)) return href;
  if (!/^https?:\/\//i.test(href)) return null;
  try {
    const url = new URL(href);
    return url.protocol === "http:" || url.protocol === "https:" ? url.href : null;
  } catch {
    return null;
  }
}

/**
 * What someone typed or pasted as a link: a doc (its id, `#cv-…`, or a
 * Docs address with one in its hash) links as `#cv-…`; `example.com`
 * gains https://; an address is a mailto. Null when it isn't a link.
 */
export function linkFrom(text: string): string | null {
  const t = text.trim();
  if (!t) return null;
  if (isDocId(t)) return `#${t}`;
  const hash = /^https?:\/\/docs\.[^/#]*\/?#(cv-[0-9a-f]{32})$/i.exec(t);
  if (hash?.[1]) return `#${hash[1]}`;
  if (/^[^\s@/]+@[^\s@/]+\.[^\s@/]+$/.test(t)) return safeHref(`mailto:${t}`);
  if (/^[a-z0-9-]+(\.[a-z0-9-]+)+(\/\S*)?$/i.test(t)) return safeHref(`https://${t}`);
  return safeHref(t);
}

/** The artifact a pasted Artifacts link or id names, or null. */
export function artifactFrom(text: string): string | null {
  const t = text.trim();
  if (isArtifactId(t)) return t;
  const m = /^(?:https?:\/\/[^/#]+\/?)?#(cv-[0-9a-f]{32})$/i.exec(t) ?? /^https?:\/\/([0-9a-f]{32})\.artifact\.[^/]+\/?/i.exec(t);
  const id = m?.[1] ? (m[1].startsWith("cv-") ? m[1] : `cv-${m[1]}`).toLowerCase() : null;
  return isArtifactId(id) ? id : null;
}

/**
 * The Artifacts app's page for artifact `id`: this host with its leading
 * `<app>.` swapped for `artifacts.`, the same port, the id in the hash
 * (as Chat's card). Null for anything that isn't an artifact's id.
 */
export function artifactsUrl(here: { protocol: string; host: string }, id: string): string | null {
  const app = /^[a-z][a-z0-9-]*\./;
  if (!isArtifactId(id) || !app.test(here.host)) return null;
  return `${here.protocol}//${here.host.replace(app, "artifacts.")}/#${id}`;
}

// --- What the page sends ---------------------------------------------

export interface Invitee {
  login: string;
  share: Share;
}

export interface CreateForm {
  title: string;
  label: Label | null;
  people: readonly Invitee[];
}

/**
 * `doc.create`'s input: everyone in `with`, and those who only comment or
 * view named again (ADR 0035 §2: membership is fixed when a doc is made).
 */
export function createInput(form: CreateForm): { input: Record<string, unknown> } | { problem: string } {
  const title = form.title.trim();
  if (!title) return { problem: "Give it a title." };
  if (/[\u0000-\u001f\u007f]/.test(title) || [...title].length > MAX_TITLE_CHARS) return { problem: `A title is one line of up to ${MAX_TITLE_CHARS} characters.` };
  const seen = new Map<string, Share>();
  for (const p of form.people) if (p.login) seen.set(p.login, p.share);
  const input: Record<string, unknown> = { title, with: [...seen.keys()] };
  if (form.label) {
    if (!DOC_LABELS.includes(form.label)) return { problem: "Pick Public, Internal or Confidential." };
    input.label = form.label;
  }
  const commenters = [...seen].filter(([, s]) => s === "commenter").map(([l]) => l);
  const viewers = [...seen].filter(([, s]) => s === "viewer").map(([l]) => l);
  if (commenters.length) input.commenters = commenters;
  if (viewers.length) input.viewers = viewers;
  return { input };
}

/** `doc.roles`: who edits and who comments; everyone else in the doc views. The owner isn't named. */
export function rolesInput(doc: string, owner: string, people: readonly { login: string; role?: Role }[]): Record<string, unknown> {
  const others = people.filter((p) => p.login !== owner && p.role !== "owner");
  return {
    doc,
    editors: others.filter((p) => p.role === "editor").map((p) => p.login),
    commenters: others.filter((p) => p.role === "commenter").map((p) => p.login),
  };
}

/** `doc.comment`: text, and where it is or what it answers. */
export function commentInput(doc: string, text: string, at: { path?: string | null; replyTo?: string | null } = {}): { input: Record<string, unknown> } | { problem: string } {
  const t = text.trim();
  if (!t) return { problem: "Write something first." };
  if (t.length > MAX_TEXT_CHARS) return { problem: `A comment is up to ${MAX_TEXT_CHARS} characters.` };
  const input: Record<string, unknown> = { doc, text: t };
  if (at.replyTo) input.reply_to = at.replyTo;
  else if (at.path) input.anchor = { path: at.path };
  return { input };
}

/** `doc.version`: a name for the doc as it is now (DC-3). */
export function versionInput(doc: string, name: string): { input: Record<string, unknown> } | { problem: string } {
  const n = name.trim();
  if (!n) return { problem: "Name it, so people can tell versions apart." };
  if (/[\u0000-\u001f\u007f]/.test(n) || [...n].length > MAX_VERSION_CHARS) return { problem: `A version's name is one line of up to ${MAX_VERSION_CHARS} characters.` };
  return { input: { doc, name: n } };
}

/** Comments with their replies under them, in order. A reply to nothing listed stands alone. */
export function threads(comments: readonly Comment[]): { comment: Comment; replies: Comment[] }[] {
  const ids = new Set(comments.map((c) => c.id));
  const top = comments.filter((c) => !c.reply_to || !ids.has(c.reply_to));
  return top.map((comment) => ({ comment, replies: comments.filter((c) => c.reply_to === comment.id && c !== comment) }));
}

/** The open threads with anchors: the ones the document highlights. */
export function anchored(comments: readonly Comment[]): Comment[] {
  return comments.filter((c) => !c.reply_to && !c.resolved && typeof c.anchor?.path === "string" && c.anchor.path.length > 0);
}

/**
 * The word a comment at `offset` in a block's text marks: from there to
 * the word's end (skipping spaces and punctuation first), or the last
 * word when nothing follows; null when the block has no word. An anchor
 * is one position (ADR 0035 §2), so this is what is highlighted.
 */
export function wordAt(text: string, offset: number): [number, number] | null {
  const word = (i: number) => WORD.test(text[i] ?? "");
  let start = Math.max(0, Math.min(offset, text.length));
  while (start < text.length && !word(start)) start++;
  if (start >= text.length) {
    let end = Math.min(offset, text.length);
    while (end > 0 && !word(end - 1)) end--;
    let begin = end;
    while (begin > 0 && word(begin - 1)) begin--;
    return end > begin ? [begin, end] : null;
  }
  let end = start;
  while (end < text.length && word(end)) end++;
  return [start, end];
}

const WORD = /[\p{L}\p{N}_'’-]/u;

/** A comment's quote of what it's about: one line, cut at `max` characters. */
export function snippet(text: string, max = 80): string {
  const line = text.replace(/\s+/g, " ").trim();
  return [...line].length > max ? `${[...line].slice(0, max - 1).join("").trimEnd()}…` : line;
}

// --- The slash menu ----------------------------------------------------

export type BlockKind =
  | "paragraph"
  | "h1"
  | "h2"
  | "h3"
  | "bullet"
  | "ordered"
  | "task"
  | "quote"
  | "code"
  | "info"
  | "warn"
  | "success"
  | "rule"
  | "table"
  | "doc-link"
  | "artifact";

export interface SlashItem {
  kind: BlockKind;
  title: string;
  hint: string;
  words: string;
}

/** What "/" offers, in order (DC-1). */
export const SLASH: readonly SlashItem[] = [
  { kind: "paragraph", title: "Text", hint: "Plain text", words: "text paragraph plain" },
  { kind: "h1", title: "Heading 1", hint: "#", words: "heading title h1 big" },
  { kind: "h2", title: "Heading 2", hint: "##", words: "heading subtitle h2" },
  { kind: "h3", title: "Heading 3", hint: "###", words: "heading h3 small" },
  { kind: "bullet", title: "Bulleted list", hint: "-", words: "bullet list unordered ul" },
  { kind: "ordered", title: "Numbered list", hint: "1.", words: "numbered ordered list ol" },
  { kind: "task", title: "Checklist", hint: "[ ]", words: "checklist todo task check box" },
  { kind: "quote", title: "Quote", hint: ">", words: "quote blockquote" },
  { kind: "code", title: "Code", hint: "```", words: "code block snippet pre" },
  { kind: "info", title: "Callout", hint: "Info", words: "callout info note" },
  { kind: "warn", title: "Warning", hint: "Callout", words: "callout warning warn caution" },
  { kind: "success", title: "Success", hint: "Callout", words: "callout success done tip" },
  { kind: "rule", title: "Divider", hint: "---", words: "divider rule line hr separator" },
  { kind: "table", title: "Table", hint: "3 × 3", words: "table grid rows columns" },
  { kind: "doc-link", title: "Link to a doc", hint: "", words: "link doc page mention" },
  { kind: "artifact", title: "Artifact", hint: "Embed", words: "artifact embed page chart" },
];

/** The items whose title or words start with each word of `query`, in order. */
export function slashMatches(query: string): SlashItem[] {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (!words.length) return [...SLASH];
  return SLASH.filter((item) => {
    const mine = `${item.title} ${item.words}`.toLowerCase().split(/[\s-]+/);
    return words.every((w) => mine.some((m) => m.startsWith(w)));
  });
}
