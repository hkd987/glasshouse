// Artifacts' logic (AR-1 to AR-5, ADR 0033), kept apart from the page so
// the unit tests cover it without a DOM: where an artifact's own origin is,
// what a frame may tell the app (every field untrusted), which comments get
// pins and where, and what a share sends.

export interface Person {
  login: string;
  name: string;
}

/** What an artifact's data is labeled (DP): who may see it, and what may read it. */
export type Label = "public" | "internal" | "confidential" | "restricted";

/** The labels a person may share at from this app (its manifest's), most open first. */
export const SHARE_LABELS: readonly Label[] = ["public", "internal", "confidential"];

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

/** One row of `artifact.list`. */
export interface Row {
  id: string;
  title: string;
  label?: Label;
  owner: string;
  owner_name: string;
  mine: boolean;
  people: Person[];
  version: number | null;
  updated_at: string | null;
  open_comments: number;
}

export interface Version {
  number: number;
  note: string;
  by: string;
  by_name: string;
  attribution: string | null;
  at: string;
  bytes: number;
}

/** Where a comment is pinned: an element path from the page's body, or a point in thousandths of the page. */
export interface Anchor {
  path?: string | null;
  x?: number | null;
  y?: number | null;
}

export interface Comment {
  id: string;
  by: string;
  by_name: string;
  attribution: string | null;
  text: string;
  version: number;
  anchor: Anchor | null;
  reply_to: string | null;
  resolved: boolean;
  at: string;
}

/** `artifact.read`. */
export interface Artifact {
  id: string;
  title: string;
  label?: Label;
  owner: string;
  owner_name: string;
  mine: boolean;
  people: Person[];
  version: number;
  versions: Version[];
  comments: Comment[];
}

/** An artifact's id: its conversation's, `cv-` and 32 hex digits. */
export const ID = /^cv-[0-9a-f]{32}$/;

/** The longest element path the frame may name (the shim's, and `artifact.comment`'s). */
export const MAX_PATH = 512;
/** The most paths asked about, and rects heard, at once. */
export const MAX_RECTS = 200;
/** The largest page `artifact.share` keeps (1.5 MB, a sealed blob). */
export const MAX_PAGE_BYTES = 1536 * 1024;
/** What the app's bridge takes in one request (its MAX_BODY, 4 MiB). */
export const MAX_REQUEST_BYTES = 4 * 1024 * 1024;
/** A comment's longest text (chat's MAX_TEXT_CHARS). */
export const MAX_TEXT_CHARS = 8000;
/** A title's longest (MAX_TITLE_CHARS). */
export const MAX_TITLE_CHARS = 200;
/** A pin's size on screen, in CSS pixels. */
export const PIN = 22;

export function isArtifactId(value: unknown): value is string {
  return typeof value === "string" && ID.test(value);
}

/**
 * An artifact's page on its own origin (ADR 0033): this app's host with
 * its leading `<app>.` swapped for `<hex>.artifact.`, the same port.
 * `version` names one (`/v<n>`); without it, the latest (`/`). Null when
 * the id isn't an artifact's or this host doesn't name an app.
 */
export function artifactUrl(here: { protocol: string; host: string }, id: string, version?: number | null): string | null {
  if (!isArtifactId(id)) return null;
  const app = /^[a-z][a-z0-9-]*\./;
  if (!app.test(here.host)) return null;
  const host = here.host.replace(app, `${id.slice(3)}.artifact.`);
  const path = version !== undefined && version !== null && Number.isInteger(version) && version > 0 ? `/v${version}` : "/";
  return `${here.protocol}//${host}${path}`;
}

/** The artifact a location's hash names (`#cv-<hex>`), if any. */
export function deepLink(hash: string): string | null {
  const id = hash.replace(/^#/, "");
  return isArtifactId(id) ? id : null;
}

/** What was shared with the person, and their own, each newest first. */
export function partition(rows: readonly Row[]): { shared: Row[]; mine: Row[] } {
  const newest = (a: Row, b: Row) => (b.updated_at ?? "").localeCompare(a.updated_at ?? "") || a.title.localeCompare(b.title);
  return {
    shared: rows.filter((r) => !r.mine).sort(newest),
    mine: rows.filter((r) => r.mine).sort(newest),
  };
}

// What a frame tells the app (the bridge's artifact-shim.js). The frame
// runs whatever the artifact says, so every field is checked here and
// anything else is dropped.

export interface Rect {
  path: string;
  x: number;
  y: number;
  w: number;
  h: number;
}

export type FrameMessage =
  | { type: "ready"; height: number }
  | { type: "pick"; path: string | null; x: number; y: number }
  | { type: "rects"; rects: (Rect | null)[] };

function record(value: unknown): Record<string, unknown> | null {
  return typeof value === "object" && value !== null && !Array.isArray(value) ? (value as Record<string, unknown>) : null;
}

/** A finite number clamped to [min, max], or null. */
function finite(value: unknown, min: number, max: number): number | null {
  return typeof value === "number" && Number.isFinite(value) ? Math.min(max, Math.max(min, value)) : null;
}

/** An element path: one line of at most MAX_PATH characters. */
export function elementPath(value: unknown): string | null {
  return typeof value === "string" && value.length > 0 && value.length <= MAX_PATH && !/[\u0000-\u001f\u007f]/.test(value) ? value : null;
}

/** Where things are in the frame's viewport: far enough out is far enough. */
const FAR = 100_000;

function rect(value: unknown): Rect | null {
  const r = record(value);
  if (!r) return null;
  const path = elementPath(r.path);
  const x = finite(r.x, -FAR, FAR);
  const y = finite(r.y, -FAR, FAR);
  const w = finite(r.w, 0, FAR);
  const h = finite(r.h, 0, FAR);
  if (path === null || x === null || y === null || w === null || h === null) return null;
  return { path, x, y, w, h };
}

/** A message from an artifact's frame, checked; null for anything else. */
export function readFrameMessage(data: unknown): FrameMessage | null {
  const m = record(data);
  if (!m || m.glass !== "artifact") return null;
  switch (m.type) {
    case "ready":
      return { type: "ready", height: finite(m.height, 0, 10_000_000) ?? 0 };
    case "pick": {
      if (m.path !== null && elementPath(m.path) === null) return null;
      const x = finite(m.x, 0, 1000);
      const y = finite(m.y, 0, 1000);
      if (x === null || y === null) return null;
      return { type: "pick", path: m.path === null ? null : (m.path as string), x: Math.round(x), y: Math.round(y) };
    }
    case "rects":
      if (!Array.isArray(m.rects)) return null;
      return { type: "rects", rects: m.rects.slice(0, MAX_RECTS).map(rect) };
    default:
      return null;
  }
}

/** A pin: an open comment on the version shown, at an element. */
export interface Pinned {
  n: number;
  comment: Comment;
  path: string;
}

/**
 * The comments that get pins on version `shown`: open, not replies, made
 * on that version, at an element. Numbered in order; the same numbers
 * label them in the list.
 */
export function pinned(comments: readonly Comment[], shown: number): Pinned[] {
  const pins: Pinned[] = [];
  for (const comment of comments) {
    const path = elementPath(comment.anchor?.path);
    if (comment.reply_to || comment.resolved || comment.version !== shown || path === null) continue;
    pins.push({ n: pins.length + 1, comment, path });
  }
  return pins.slice(0, MAX_RECTS);
}

/** The paths to ask the frame about: the pins', then one being picked. Never anything else. */
export function wherePaths(pins: readonly Pinned[], picking: string | null): string[] {
  const paths = [...new Set([...pins.map((p) => p.path), ...(picking ? [picking] : [])])];
  return paths.slice(0, MAX_RECTS);
}

/**
 * Where a pin goes over the frame: at the element's top left corner, kept
 * inside the frame; null when the element is scrolled out of sight.
 */
export function pinPosition(r: Rect, frame: { w: number; h: number }): { left: number; top: number } | null {
  if (r.y + r.h < 0 || r.y > frame.h || r.x + r.w < 0 || r.x > frame.w) return null;
  const clamp = (v: number, max: number) => Math.round(Math.min(Math.max(0, max - PIN), Math.max(0, v)));
  return { left: clamp(r.x - PIN / 2, frame.w), top: clamp(r.y - PIN / 2, frame.h) };
}

/** Comments with their replies under them, in order. A reply to nothing listed stands alone. */
export function threads(comments: readonly Comment[]): { comment: Comment; replies: Comment[] }[] {
  const ids = new Set(comments.map((c) => c.id));
  const top = comments.filter((c) => !c.reply_to || !ids.has(c.reply_to));
  return top.map((comment) => ({ comment, replies: comments.filter((c) => c.reply_to === comment.id && c !== comment) }));
}

/** "on v1" when a comment was made on another version than the one shown (AR-4). */
export function versionBadge(comment: Comment, shown: number): string | null {
  return comment.version !== shown ? `on v${comment.version}` : null;
}

/** Never the base system's name on screen (BR-1), whatever a result says. */
export function readable(text: string): string {
  return text.replace(/free\s*bsd/gi, "the system");
}

/** What a page from the person weighs, in bytes. */
export function bytes(text: string): number {
  return new TextEncoder().encode(text).length;
}

export function size(n: number): string {
  return n < 1024 ? `${n} bytes` : n < 1024 * 1024 ? `${Math.ceil(n / 1024)} KB` : `${Number((n / (1024 * 1024)).toFixed(1))} MB`;
}

export interface ShareForm {
  title: string;
  /** Internal unless said (the action's default). */
  label?: Label | null;
  html: string;
  with: string[];
  conversation: string | null;
  note: string;
}

/**
 * `artifact.share`'s input from the share form, or why it can't go: no
 * title, no page, or a page larger than the bridge takes in one request.
 */
export function shareInput(form: ShareForm): { input: Record<string, unknown> } | { problem: string } {
  const title = form.title.trim();
  if (!title) return { problem: "Give it a title." };
  if (/[\u0000-\u001f\u007f]/.test(title) || [...title].length > MAX_TITLE_CHARS) return { problem: `A title is one line of up to ${MAX_TITLE_CHARS} characters.` };
  if (!form.html.trim()) return { problem: "Choose an .html file or paste the page." };
  const input: Record<string, unknown> = { title, html: form.html, with: [...new Set(form.with)] };
  if (form.label) {
    if (!SHARE_LABELS.includes(form.label)) return { problem: "Pick Public, Internal or Confidential." };
    input.label = form.label;
  }
  if (form.conversation) {
    // A chat conversation's id has the same shape as an artifact's.
    if (!ID.test(form.conversation)) return { problem: "That conversation can't be found." };
    input.conversation = form.conversation;
  }
  if (form.note.trim()) input.note = form.note.trim();
  const problem = tooLarge("artifact.share", input);
  return problem ? { problem } : { input };
}

/** `artifact.update`'s input: a page and a note saying what changed. */
export function updateInput(artifact: string, html: string, note: string): { input: Record<string, unknown> } | { problem: string } {
  if (!html.trim()) return { problem: "Choose an .html file or paste the page." };
  if (!note.trim()) return { problem: "Say what changed, for the people who see it." };
  const input = { artifact, html, note: note.trim() };
  const problem = tooLarge("artifact.update", input);
  return problem ? { problem } : { input };
}

/** Why a request is too large to send, or null when it fits. */
export function tooLarge(action: string, input: unknown): string | null {
  const page = record(input)?.html;
  if (typeof page === "string" && bytes(page) > MAX_PAGE_BYTES) {
    return `This page is ${size(bytes(page))}; a page is ${size(MAX_PAGE_BYTES)} at most.`;
  }
  const request = bytes(JSON.stringify({ action, input }));
  if (request > MAX_REQUEST_BYTES) {
    return `This page is ${size(request)} as sent; pages shared from this app are up to ${size(MAX_REQUEST_BYTES)} for now. Ask your agent to share a larger one from its file.`;
  }
  return null;
}

/** An HTML file someone chose or dropped: by its name or type. */
export function isHtmlFile(file: { name: string; type: string }): boolean {
  return /\.html?$/i.test(file.name) || file.type === "text/html";
}

/** "just now", "5 min ago", "3 h ago", then the date. */
export function when(at: string | null, now: Date = new Date()): string {
  if (!at) return "";
  const d = new Date(at);
  if (Number.isNaN(d.getTime())) return "";
  const minutes = Math.floor((now.getTime() - d.getTime()) / 60_000);
  if (minutes < 1) return "just now";
  if (minutes < 60) return `${minutes} min ago`;
  if (minutes < 24 * 60) return `${Math.floor(minutes / 60)} h ago`;
  return d.toLocaleDateString("en-US", { month: "short", day: "numeric" });
}

export function initials(name: string): string {
  const parts = name.trim().split(/\s+/);
  return ((parts[0]?.[0] ?? "") + (parts.length > 1 ? (parts[parts.length - 1]?.[0] ?? "") : "")).toUpperCase() || "?";
}
