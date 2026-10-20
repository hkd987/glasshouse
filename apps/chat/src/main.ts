// Chat (CF-91, CF-92, CF-93, ADR 0030): channels and direct messages with
// people in the org, sealed end to end between their computers. glassd
// seals and opens everything; this page shows what it is given and keeps
// nothing: every call is `bridge.call`, never `bridge.read`, so no message
// lands in the page's storage. Drafts and notification levels live in
// glassd too, through actions. Live updates come from `app.status` waits,
// which carry ids only, so the audit log gets one read per change rather
// than per poll.
//
// Like Slack, inside the seal (C4a): threads in a side panel, reactions,
// edits and deletes of the person's own messages, pins, channel topics,
// @mentions, unread markers, drafts, search and a level per conversation.
// Message text only ever goes in as text nodes (h() and show()).

import {
  attributionChip,
  type AppCallResult,
  Bridge,
  BridgeError,
  h,
  Offline,
  parseAttribution,
  ready,
  show,
  showAttribution,
  stepHighlight,
} from "@glasshouse/sdk";

import {
  type Conversation,
  type ConversationView,
  type Hit,
  type Level,
  type Message,
  type Person,
  type Shared,
  ago,
  dayLabel,
  DRAFT_DELAY_MS,
  draftKey,
  editable,
  emojiName,
  firstUnread,
  groupHits,
  initials,
  insertMention,
  joins,
  lastEditable,
  LEVELS,
  matchSegments,
  mentionMatches,
  mentionQuery,
  mentionSegments,
  mentionsMe,
  MORE_EMOJI,
  newDay,
  QUICK_EMOJI,
  reactionPills,
  rowBadges,
  threadSummary,
  TOPIC_MAX,
} from "./chat.ts";

interface Found {
  login: string;
  name: string;
  email: string;
  title?: string;
}

/** Choosing people: for a new message, a new channel's members, or people to add. */
interface Picker {
  query: string;
  found: Found[];
  picked: Found[];
  /** Logins not to suggest: who is in the conversation already. */
  exclude?: string[];
}

type Mode =
  | { kind: "conversation"; id: string }
  | { kind: "new-message"; to: Picker }
  | { kind: "new-channel"; name: string; to: Picker }
  | { kind: "search"; query: string; hits: Hit[] | null; back: string | null }
  | { kind: "none" };

/** The side panel: one thread, or the conversation's pinned messages. */
type Panel = { kind: "thread"; root: string; messages: Message[]; loaded: boolean; typed: boolean } | { kind: "pins" } | null;

/** Where a message is drawn: the timeline of roots, or the thread panel. */
type Where = "timeline" | "thread";

const bridge = new Bridge({ app: "chat" });
const state = {
  me: "",
  conversations: [] as Conversation[],
  mode: { kind: "none" } as Mode,
  /** The open conversation's timeline: thread roots, oldest first. */
  messages: [] as Message[],
  moreBefore: false,
  /** `chat.read`'s view of the open conversation: topic, pins, level, draft. */
  view: null as ConversationView | null,
  /** The first unread message when the conversation opened, for "New messages". */
  divider: null as string | null,
  panel: null as Panel,
  editing: null as { id: string; in: Where } | null,
  topicEditing: false,
  /** A message to point at (from search or a pin), and one to scroll to on the next draw. */
  highlight: null as string | null,
  scrollTo: null as string | null,
  /** Keep the timeline at its newest on the next draw. */
  stick: true,
  /** What is being sent now, shown until it is posted or refused. */
  sending: null as { text: string; waiting: string | null; thread: string | null } | null,
  notice: null as { text: string; tone: "bad" | "ok" } | null,
  fingerprint: null as string | null,
  offline: false,
  cursor: 0,
  /** The message the arrow keys move from in each list. */
  focused: { timeline: null as string | null, thread: null as string | null },
  /** Typed into the open conversation's composer since it opened. */
  typed: false,
  /** Adding people to the open channel or group (`chat.members`). */
  adding: null as { to: Picker; busy: boolean; problem: string | null } | null,
};

/** Every message seen while the page is open, for the pinned panel. In memory only. */
const known = new Map<string, Message>();

/**
 * Drafts (CF-92): what each composer holds, kept in glassd with
 * `chat.draft` once typing rests, per conversation and per thread.
 */
const drafts = {
  local: new Map<string, string>(),
  saved: new Map<string, string>(),
  timers: new Map<string, ReturnType<typeof setTimeout>>(),
};

// --- Icons: a few strokes, drawn as SVG nodes (never markup) -----------------------

const SVG = "http://www.w3.org/2000/svg";
const circle = (cx: number, cy: number, r: number) => `M${cx - r} ${cy}a${r} ${r} 0 1 0 ${2 * r} 0a${r} ${r} 0 1 0 ${-2 * r} 0`;
const BELL = ["M6.5 16.5V11a5.5 5.5 0 0 1 11 0v5.5l1.5 1.5h-14z", "M10 20.5a2 2 0 0 0 4 0"];
const ICONS = {
  smile: [circle(12, 12, 8.5), "M8.6 14.2c.9 1.2 2 1.8 3.4 1.8s2.5-.6 3.4-1.8", "M9.2 9.6v.6", "M14.8 9.6v.6"],
  thread: ["M20 14.5a1.5 1.5 0 0 1-1.5 1.5H9l-5 4V5.5A1.5 1.5 0 0 1 5.5 4h13A1.5 1.5 0 0 1 20 5.5z", "M8 8.5h8", "M8 11.5h5"],
  pin: ["M9 3.5h6", "M10 3.5v5.5L7 13h10l-3-4V3.5", "M12 13v7.5"],
  pencil: ["M15.5 4.5l4 4L9 19l-5 1 1-5z", "M13.5 6.5l4 4"],
  trash: ["M4.5 7h15", "M9.5 7V4.5h5V7", "M6.5 7l1 12.5h9l1-12.5", "M10 11v5", "M14 11v5"],
  bell: BELL,
  bellOff: [...BELL, "M4 4l16 16"],
  at: [circle(12, 12, 3.5), "M15.5 12v1.5a2.5 2.5 0 0 0 5 0V12a8.5 8.5 0 1 0-3.5 6.9"],
  search: [circle(11, 11, 6.5), "M16 16l4.5 4.5"],
  close: ["M6.5 6.5l11 11", "M17.5 6.5l-11 11"],
  plus: ["M12 5.5v13", "M5.5 12h13"],
  check: ["M5 12.5l4.5 4.5L19 7.5"],
  people: [circle(9, 8.5, 3.5), "M3 19.5c.6-3.2 3-5 6-5s5.4 1.8 6 5", "M15.5 5.3a3.3 3.3 0 0 1 0 6.4", "M17.5 14.8c1.8.6 3 2.2 3.5 4.7"],
};

function icon(name: keyof typeof ICONS): SVGSVGElement {
  const svg = document.createElementNS(SVG, "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("aria-hidden", "true");
  svg.setAttribute("class", "icon");
  for (const d of ICONS[name]) {
    const path = document.createElementNS(SVG, "path");
    path.setAttribute("d", d);
    svg.append(path);
  }
  return svg;
}

// --- The page's lasting parts ---------------------------------------------------------

const root = document.getElementById("app") as HTMLElement;
// The parts that live updates redraw; the composers, the search box and
// the fields being edited stay, so typing is never interrupted.
const sideLists = h("div", { class: "side-lists" });
const head = h("header", { class: "pane-head" });
const banner = h("div", { class: "banners" });
const body = h("div", { class: "pane-body" });
const panelHead = h("header", { class: "panel-head" });
const panelBody = h("div", { class: "panel-body" });
/** Popovers, menus and the delete dialog, over everything. */
const layer = h("div", { class: "layer" });
/** True while the page redraws: a field leaving the page then isn't someone leaving it. */
let rendering = false;
/** Keep the thread at its newest reply on the next draw: after replying. */
let stickThread = false;

function grow(field: HTMLTextAreaElement, max = 180): void {
  field.style.height = "auto";
  field.style.height = `${Math.min(field.scrollHeight, max)}px`;
}

const text = h("textarea", {
  class: "composer-text",
  name: "message",
  rows: 1,
  "aria-label": "Message",
  "aria-autocomplete": "list",
  onkeydown: (e: Event) => composerKeys(e as KeyboardEvent, "timeline"),
  oninput: () => {
    grow(text);
    updateMentions(text);
    const id = openId();
    if (id) {
      state.typed = true;
      typed(draftKey(id), id, null, text.value);
    }
  },
  onclick: () => updateMentions(text),
  onblur: () => closeMentions(text),
});
const hint = h("div", { class: "composer-hint" });
const composer = h(
  "form",
  {
    class: "composer",
    "data-test": "composer",
    onsubmit: (e: Event) => {
      e.preventDefault();
      void send("timeline");
    },
  },
  text,
  h("button", { class: "button primary", type: "submit" }, "Send"),
);

/** The thread panel's composer: posts with `reply_to` set to the root. */
const replyText = h("textarea", {
  class: "composer-text",
  name: "reply",
  rows: 1,
  placeholder: "Reply…",
  "aria-label": "Reply in thread",
  "aria-autocomplete": "list",
  onkeydown: (e: Event) => composerKeys(e as KeyboardEvent, "thread"),
  oninput: () => {
    grow(replyText);
    updateMentions(replyText);
    const id = openId();
    const panel = state.panel;
    if (id && panel?.kind === "thread") {
      panel.typed = true;
      typed(draftKey(id, panel.root), id, panel.root, replyText.value);
    }
  },
  onclick: () => updateMentions(replyText),
  onblur: () => closeMentions(replyText),
});
const replyComposer = h(
  "form",
  {
    class: "composer",
    "data-test": "reply-composer",
    onsubmit: (e: Event) => {
      e.preventDefault();
      void send("thread");
    },
  },
  replyText,
  h("button", { class: "button primary", type: "submit" }, "Reply"),
);
const replyWrap = h("div", { class: "composer-wrap" }, replyComposer, h("div", { class: "composer-hint" }, "Enter to reply · Shift+Enter for a new line"));
const panel = h("aside", { class: "side-panel", "aria-label": "Side panel", hidden: true }, panelHead, panelBody, replyWrap);

/** The message being edited in place. */
const editField = h("textarea", {
  class: "edit-text",
  name: "edit",
  rows: 1,
  "aria-label": "Edit message",
  "aria-autocomplete": "list",
  onkeydown: (e: Event) => {
    const key = e as KeyboardEvent;
    if (mentionKeys(key, editField)) return;
    if (key.key === "Enter" && !key.shiftKey && !key.isComposing) {
      e.preventDefault();
      void saveEdit();
    } else if (key.key === "Escape") {
      e.preventDefault();
      cancelEdit();
    }
  },
  oninput: () => {
    grow(editField, 260);
    updateMentions(editField);
  },
  onblur: () => closeMentions(editField),
});

const topicCount = h("span", { class: "topic-hint" });
const topicField = h("input", {
  class: "topic-field",
  name: "topic",
  maxlength: TOPIC_MAX,
  autocomplete: "off",
  placeholder: "What this channel is for",
  "aria-label": "Channel topic",
  onkeydown: (e: Event) => {
    const key = e as KeyboardEvent;
    if (key.key === "Enter") {
      e.preventDefault();
      void saveTopic();
    } else if (key.key === "Escape") {
      e.preventDefault();
      cancelTopic();
    }
  },
  oninput: () => countTopic(),
  onblur: () => {
    if (!rendering && state.topicEditing) void saveTopic();
  },
});

function countTopic(): void {
  show(topicCount, `${topicField.value.length}/${TOPIC_MAX} · Enter to save · Esc to cancel`);
}

let searchTimer: ReturnType<typeof setTimeout> | undefined;
const searchField = h("input", {
  type: "search",
  name: "search",
  autocomplete: "off",
  placeholder: "Search messages",
  "aria-label": "Search messages",
  oninput: () => {
    clearTimeout(searchTimer);
    const query = searchField.value;
    if (!query.trim()) {
      leaveSearch();
      return;
    }
    if (query.trim().length >= 2) searchTimer = setTimeout(() => void runSearch(query), 350);
  },
  onkeydown: (e: Event) => {
    const key = e as KeyboardEvent;
    if (key.key === "Enter") {
      e.preventDefault();
      clearTimeout(searchTimer);
      if (searchField.value.trim()) void runSearch(searchField.value);
    } else if (key.key === "Escape") {
      e.preventDefault();
      clearTimeout(searchTimer);
      searchField.value = "";
      leaveSearch();
    } else if (key.key === "ArrowDown" && state.mode.kind === "search") {
      e.preventDefault();
      body.querySelector<HTMLElement>("[data-hit]")?.focus();
    }
  },
});

function failure(error: unknown, doing: string): string {
  if (error instanceof Offline) return `${doing} needs the system, and it isn't answering.`;
  if (error instanceof BridgeError) return error.message;
  return String(error);
}

/** Says something went through; it goes by itself after a few seconds. */
function sayDone(text: string): void {
  const notice = { text, tone: "ok" as const };
  state.notice = notice;
  setTimeout(() => {
    if (state.notice === notice) {
      state.notice = null;
      render();
    }
  }, 5000);
}

function openId(): string | null {
  return state.mode.kind === "conversation" ? state.mode.id : null;
}

function openRow(): Conversation | undefined {
  const id = openId();
  return id ? state.conversations.find((c) => c.id === id) : undefined;
}

/** The person's name as their conversation shows it. */
function myName(): string {
  for (const c of state.conversations) {
    const me = c.people.find((p) => p.login === state.me);
    if (me) return me.name;
  }
  return state.me;
}

function remember(messages: readonly Message[]): void {
  for (const m of messages) known.set(m.id, m);
}

/** Every copy of message `id` the page holds: the timeline's, the thread's and the one remembered. */
function copies(id: string): Message[] {
  const out = state.messages.filter((m) => m.id === id);
  if (state.panel?.kind === "thread") out.push(...state.panel.messages.filter((m) => m.id === id));
  const kept = known.get(id);
  if (kept && !out.includes(kept)) out.push(kept);
  return out;
}

function find(id: string): Message | undefined {
  return copies(id)[0];
}

function threadMessages(): Message[] {
  return state.panel?.kind === "thread" ? state.panel.messages : [];
}

/** Runs one action the person asked for: says when the desktop asks, and shows a refusal. */
async function act<T>(doing: string, asks: boolean, call: () => Promise<AppCallResult<T>>): Promise<T | null> {
  if (asks) {
    state.notice = { text: `${doing}: waiting for your approval on the desktop.`, tone: "ok" };
    render();
  }
  try {
    const result = await call();
    if (asks) state.notice = null;
    if (!result.ok) {
      state.notice = { text: result.text, tone: "bad" };
      render();
      return null;
    }
    return result.data;
  } catch (error) {
    state.notice = { text: failure(error, doing), tone: "bad" };
    render();
    return null;
  }
}

// --- Loading --------------------------------------------------------------------------

async function loadList(): Promise<void> {
  try {
    const result = await bridge.call<{ conversations: Conversation[]; me: string; compliance: string | null }>("chat.list", {});
    state.offline = false;
    if (!result.ok) {
      state.notice = { text: result.text, tone: "bad" };
      return;
    }
    state.conversations = result.data.conversations;
    state.me = result.data.me;
    state.fingerprint = result.data.compliance;
  } catch (error) {
    state.offline = error instanceof Offline;
    state.notice = { text: failure(error, "Chat"), tone: "bad" };
  }
}

interface Read {
  conversation?: ConversationView;
  messages: Message[];
  more_before?: boolean;
  thread?: { root: string; draft: string | null };
}

/** The open conversation's timeline: as many roots as shown now (50 at least), which marks it read. */
async function loadMessages(): Promise<void> {
  const id = openId();
  if (!id) return;
  try {
    const limit = Math.min(200, Math.max(50, state.messages.length));
    const result = await bridge.call<Read>("chat.read", { conversation: id, limit });
    if (openId() !== id) return;
    if (result.ok) {
      state.messages = result.data.messages;
      state.moreBefore = result.data.more_before ?? false;
      state.view = result.data.conversation ?? null;
      remember(state.messages);
      // Reading it in Chat marked it read.
      const row = state.conversations.find((c) => c.id === id);
      if (row) {
        row.unread = 0;
        row.mentions = 0;
        if (state.view?.topic !== undefined) row.topic = state.view.topic;
        if (state.view?.notify) row.notify = state.view.notify;
      }
    } else state.notice = { text: result.text, tone: "bad" };
  } catch (error) {
    state.notice = { text: failure(error, "Reading"), tone: "bad" };
  }
}

/** Older roots, before the oldest shown. */
async function loadEarlier(): Promise<boolean> {
  const id = openId();
  const first = state.messages[0];
  if (!id || !first || !state.moreBefore) return false;
  try {
    const result = await bridge.call<Read>("chat.read", { conversation: id, before: first.id, limit: 100 });
    if (openId() !== id || !result.ok) return false;
    const have = new Set(state.messages.map((m) => m.id));
    state.messages = [...result.data.messages.filter((m) => !have.has(m.id)), ...state.messages];
    state.moreBefore = result.data.more_before ?? false;
    remember(result.data.messages);
    return true;
  } catch (error) {
    state.notice = { text: failure(error, "Reading"), tone: "bad" };
    return false;
  }
}

/** The open thread: its root, then every reply. */
async function loadThread(): Promise<void> {
  const id = openId();
  const open = state.panel;
  if (!id || open?.kind !== "thread") return;
  try {
    const result = await bridge.call<Read>("chat.read", { conversation: id, thread: open.root });
    if (state.panel !== open) return;
    if (!result.ok) {
      state.notice = { text: result.text, tone: "bad" };
      return;
    }
    const first = !open.loaded;
    open.messages = result.data.messages;
    open.loaded = true;
    remember(open.messages);
    const key = draftKey(id, open.root);
    const kept = result.data.thread?.draft ?? "";
    if (first) {
      drafts.saved.set(key, kept);
      if (!open.typed && !drafts.local.has(key) && replyText.value !== kept) {
        replyText.value = kept;
        grow(replyText);
      }
    }
  } catch (error) {
    state.notice = { text: failure(error, "Reading the thread"), tone: "bad" };
  }
}

/** After a change the person made: what it changed, without waiting for the live loop. */
async function refresh(): Promise<void> {
  await loadMessages();
  await loadThread();
  render();
}

// --- Drafts ---------------------------------------------------------------------------

/** Typing in a composer: kept in glassd once it rests for half a second. */
function typed(key: string, conversation: string, thread: string | null, value: string): void {
  drafts.local.set(key, value);
  clearTimeout(drafts.timers.get(key));
  drafts.timers.set(
    key,
    setTimeout(() => void saveDraft(key, conversation, thread), DRAFT_DELAY_MS),
  );
}

async function saveDraft(key: string, conversation: string, thread: string | null): Promise<void> {
  clearTimeout(drafts.timers.get(key));
  drafts.timers.delete(key);
  const value = drafts.local.get(key) ?? "";
  const keep = value.trim() ? value : "";
  if ((drafts.saved.get(key) ?? "") === keep) return;
  const before = drafts.saved.get(key);
  drafts.saved.set(key, keep);
  try {
    const result = await bridge.call("chat.draft", thread ? { conversation, thread, text: keep } : { conversation, text: keep });
    if (!result.ok) {
      if (before === undefined) drafts.saved.delete(key);
      else drafts.saved.set(key, before);
      return;
    }
    const row = state.conversations.find((c) => c.id === conversation);
    if (row && !thread) row.draft = keep || null;
  } catch {
    // Kept on the page until the next change; nothing typed is lost.
    if (before === undefined) drafts.saved.delete(key);
    else drafts.saved.set(key, before);
  }
}

/** Keeps a resting draft now: when leaving a conversation or a thread. */
function flushDraft(conversation: string | null, thread: string | null = null): void {
  if (!conversation) return;
  const key = draftKey(conversation, thread);
  if (drafts.timers.has(key)) void saveDraft(key, conversation, thread);
}

function flushAll(): void {
  for (const key of [...drafts.timers.keys()]) {
    const [conversation, thread] = key.split("/");
    if (conversation) void saveDraft(key, conversation, thread ?? null);
  }
}

function hasDraft(c: Conversation): boolean {
  return (drafts.local.get(c.id) ?? c.draft ?? "").trim() !== "";
}

/** Before sending: what's sent isn't a draft any more. */
function dropDraft(key: string): void {
  clearTimeout(drafts.timers.get(key));
  drafts.timers.delete(key);
  drafts.local.delete(key);
}

// --- Opening --------------------------------------------------------------------------

async function open(id: string, focus = true): Promise<void> {
  const was = openId();
  const same = was === id;
  if (!same) {
    flushDraft(was);
    if (state.panel?.kind === "thread") flushDraft(was, state.panel.root);
  }
  const row = state.conversations.find((c) => c.id === id);
  // Worked out before reading, which marks it read.
  const unread = row?.unread ?? 0;
  state.mode = { kind: "conversation", id };
  if (!same) {
    state.messages = [];
    state.view = null;
    state.moreBefore = false;
    state.panel = null;
    state.editing = null;
    state.topicEditing = false;
    state.divider = null;
    state.highlight = null;
    state.focused = { timeline: null, thread: null };
    state.typed = false;
    state.adding = null;
    const local = drafts.local.get(id);
    text.value = local ?? row?.draft ?? "";
    if (local === undefined) drafts.saved.set(id, row?.draft ?? "");
    grow(text);
  }
  state.notice = null;
  state.stick = true;
  render();
  await loadMessages();
  if (openId() !== id) return;
  if (!same) {
    state.divider = firstUnread(state.messages, unread);
    if (state.divider) state.scrollTo = state.divider;
    // What glassd kept, unless typing here has begun.
    const kept = state.view?.draft ?? null;
    if (kept !== null && !state.typed && !drafts.local.has(id) && text.value !== kept) {
      text.value = kept;
      grow(text);
    }
    if (state.view) drafts.saved.set(id, kept ?? "");
  }
  render();
  if (focus) text.focus();
}

async function openThread(rootId: string, highlight: string | null = null, focus = true): Promise<void> {
  const id = openId();
  if (!id) return;
  const current = state.panel;
  if (current?.kind === "thread" && current.root === rootId) {
    state.highlight = highlight;
    state.scrollTo = highlight;
    render();
    if (focus) replyText.focus();
    return;
  }
  if (current?.kind === "thread") flushDraft(id, current.root);
  if (state.editing?.in === "thread") state.editing = null;
  state.panel = { kind: "thread", root: rootId, messages: [], loaded: false, typed: false };
  state.focused.thread = null;
  replyText.value = drafts.local.get(draftKey(id, rootId)) ?? "";
  grow(replyText);
  state.highlight = highlight;
  render();
  await loadThread();
  state.scrollTo = highlight;
  render();
  if (focus) replyText.focus();
  if (highlight) clearHighlight(highlight);
}

function closePanel(): void {
  const id = openId();
  if (state.panel?.kind === "thread") flushDraft(id, state.panel.root);
  if (state.editing?.in === "thread") state.editing = null;
  state.panel = null;
  render();
  text.focus();
}

function togglePins(): void {
  if (state.panel?.kind === "pins") return closePanel();
  if (state.panel?.kind === "thread") flushDraft(openId(), state.panel.root);
  state.panel = { kind: "pins" };
  render();
  panelHead.querySelector<HTMLElement>("[data-act=close-panel]")?.focus();
}

function clearHighlight(id: string): void {
  setTimeout(() => {
    if (state.highlight === id) state.highlight = null;
  }, 4000);
}

/** Opens where a message is (from search or a pin) and points at it. */
async function jumpTo(conversation: string, message: string, thread: string | null): Promise<void> {
  if (openId() !== conversation) await open(conversation, false);
  if (thread) return openThread(thread, message);
  for (let tries = 0; tries < 5 && !state.messages.some((m) => m.id === message) && state.moreBefore; tries++) {
    if (!(await loadEarlier())) break;
  }
  state.highlight = message;
  state.scrollTo = message;
  state.focused.timeline = message;
  render();
  body.querySelector<HTMLElement>(`[data-message="${CSS.escape(message)}"]`)?.focus({ preventScroll: true });
  clearHighlight(message);
}

// --- Search (CF-93) -------------------------------------------------------------------

async function runSearch(query: string): Promise<void> {
  const q = query.trim();
  if (!q) return;
  const mode = state.mode;
  const back = mode.kind === "conversation" ? mode.id : mode.kind === "search" ? mode.back : null;
  if (mode.kind === "search" && mode.query === q && mode.hits) return;
  if (mode.kind === "conversation") {
    flushDraft(mode.id);
    if (state.panel?.kind === "thread") flushDraft(mode.id, state.panel.root);
  }
  state.mode = { kind: "search", query: q, hits: null, back };
  state.panel = null;
  state.editing = null;
  state.topicEditing = false;
  render();
  try {
    const result = await bridge.call<{ hits: Hit[] }>("chat.search", { query: q, limit: 50 });
    const now = state.mode;
    if (now.kind !== "search" || now.query !== q) return;
    now.hits = result.ok ? result.data.hits : [];
    if (!result.ok) state.notice = { text: result.text, tone: "bad" };
  } catch (error) {
    const now = state.mode;
    if (now.kind === "search" && now.query === q) now.hits = [];
    state.notice = { text: failure(error, "Searching"), tone: "bad" };
  }
  render();
}

function leaveSearch(): void {
  const mode = state.mode;
  if (mode.kind !== "search") return;
  state.notice = null;
  if (mode.back) void open(mode.back);
  else {
    state.mode = { kind: "none" };
    render();
  }
}

// --- People for a new message or channel ------------------------------------------------

function picker(): Picker {
  return { query: "", found: [], picked: [] };
}

async function search(p: Picker, query: string): Promise<void> {
  p.query = query;
  if (!query.trim()) {
    p.found = [];
    return render();
  }
  try {
    const result = await bridge.call<{ people: Found[] }>("people.search", { query, limit: 8 });
    if (p.query !== query) return;
    p.found = result.ok ? result.data.people.filter((f) => f.login !== state.me && !p.picked.some((x) => x.login === f.login) && !p.exclude?.includes(f.login)) : [];
  } catch {
    p.found = [];
  }
  render();
}

// --- Sending ----------------------------------------------------------------------------

/** Sends what's in a composer: to the open conversation or thread, or to the people picked. */
async function send(where: Where): Promise<void> {
  const field = where === "thread" ? replyText : text;
  const message = field.value;
  if (!message.trim() || state.sending) return;
  const mode = state.mode;
  const thread = where === "thread" && state.panel?.kind === "thread" ? state.panel.root : null;
  if (where === "thread" && !thread) return;
  let post: Record<string, unknown>;
  let conversation: string | null = null;
  if (mode.kind === "conversation") {
    conversation = mode.id;
    post = thread ? { conversation, text: message, reply_to: thread } : { conversation, text: message };
  } else if (where === "timeline" && mode.kind === "new-message" && mode.to.picked.length) post = { to: mode.to.picked.map((p) => p.login), text: message };
  else return;
  const key = conversation ? draftKey(conversation, thread) : null;
  // What's sent isn't a draft any more: cleared in glassd now, so closing
  // Chat mid-send leaves none behind. Refused, the words come back as one.
  if (key && conversation) {
    dropDraft(key);
    drafts.local.set(key, "");
    void saveDraft(key, conversation, thread);
  }
  closeMentions(field);
  state.sending = { text: message, waiting: bridge.asks("chat.post") ? "Waiting for your approval on the desktop" : null, thread };
  state.notice = null;
  state.stick = true;
  stickThread = thread !== null;
  field.value = "";
  grow(field);
  render();
  // While the desktop asks, show what it asks (app.status).
  const watch = setInterval(() => {
    bridge
      .status()
      .then((status) => {
        const asked = status.waiting ? `Waiting for your approval on the desktop: ${status.waiting.summary}` : null;
        if (state.sending && asked !== state.sending.waiting && (asked || !bridge.asks("chat.post"))) {
          state.sending.waiting = asked;
          render();
        }
      })
      .catch(() => undefined);
  }, 400);
  try {
    const result = await bridge.call<{ posted: { conversation: string }[] }>("chat.post", { posts: [post] });
    if (!result.ok) {
      // Not sent: the words stay, so nothing typed is lost.
      field.value = message;
      grow(field);
      if (key && conversation) typed(key, conversation, thread, message);
      state.notice = { text: result.text, tone: "bad" };
    } else {
      const posted = result.data.posted[0]?.conversation;
      state.sending = null;
      await loadList();
      if (posted && openId() !== posted) {
        clearInterval(watch);
        return open(posted);
      }
      await loadMessages();
      await loadThread();
    }
  } catch (error) {
    field.value = message;
    grow(field);
    if (key && conversation) typed(key, conversation, thread, message);
    state.notice = { text: `Not sent: ${failure(error, "Sending")}`, tone: "bad" };
  } finally {
    clearInterval(watch);
    state.sending = null;
  }
  render();
  field.focus();
}

async function createChannel(name: string, people: Found[]): Promise<void> {
  try {
    const result = await bridge.call<{ conversation: { id: string } }>("chat.create", {
      name,
      people: people.map((p) => p.login),
    });
    if (!result.ok) {
      state.notice = { text: result.text, tone: "bad" };
      return render();
    }
    await loadList();
    await open(result.data.conversation.id);
  } catch (error) {
    state.notice = { text: failure(error, "Making a channel"), tone: "bad" };
    render();
  }
}

function composerKeys(e: KeyboardEvent, where: Where): void {
  const field = where === "thread" ? replyText : text;
  if (mentionKeys(e, field)) return;
  if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
    e.preventDefault();
    void send(where);
  } else if (e.key === "ArrowUp" && !field.value && !e.shiftKey && !e.altKey && !e.metaKey && !e.ctrlKey) {
    // Up in an empty composer edits the person's last message there,
    // once what's being sent is in the list.
    if (state.sending) return e.preventDefault();
    const mine = lastEditable(where === "thread" ? threadMessages() : state.messages);
    if (mine) {
      e.preventDefault();
      startEdit(mine.id, where);
    }
  }
}

// --- Mentions (CF-92): @ suggests the conversation's people ----------------------------

const mentionList = h("div", { class: "popover mention-list", id: "mention-list", role: "listbox", "aria-label": "People to mention", hidden: true });
const mention = {
  field: null as HTMLTextAreaElement | null,
  at: null as { start: number; end: number; query: string } | null,
  found: [] as Person[],
  on: 0,
};

function conversationPeople(): Person[] {
  return openRow()?.people ?? [];
}

function updateMentions(field: HTMLTextAreaElement): void {
  const at = mentionQuery(field.value, field.selectionStart ?? field.value.length);
  const found = at ? mentionMatches(conversationPeople(), at.query, state.me) : [];
  if (!at || !found.length) return closeMentions(field);
  const same = mention.field === field && found.length === mention.found.length && found.every((p, i) => p.login === mention.found[i]?.login);
  mention.field = field;
  mention.at = at;
  mention.found = found;
  if (!same) mention.on = 0;
  drawMentions();
}

function drawMentions(): void {
  const field = mention.field;
  if (!field) return;
  show(
    mentionList,
    h("div", { class: "popover-title" }, "People in this conversation"),
    ...mention.found.map((p, i) =>
      h(
        "div",
        {
          class: `mention-option${i === mention.on ? " on" : ""}`,
          id: `mention-${i}`,
          role: "option",
          "aria-selected": i === mention.on ? "true" : "false",
          "data-mention": p.login,
          onmousedown: (e: Event) => {
            e.preventDefault();
            pickMention(i);
          },
        },
        h("span", { class: "avatar small" }, initials(p.name)),
        h("span", { class: "mention-name" }, p.name),
        h("span", { class: "muted" }, `@${p.login}`),
      ),
    ),
  );
  mentionList.hidden = false;
  field.setAttribute("aria-controls", "mention-list");
  field.setAttribute("aria-expanded", "true");
  field.setAttribute("aria-activedescendant", `mention-${mention.on}`);
  const rect = field.getBoundingClientRect();
  mentionList.style.left = `${Math.max(8, Math.min(rect.left, window.innerWidth - 328))}px`;
  mentionList.style.bottom = `${window.innerHeight - rect.top + 8}px`;
}

function closeMentions(field?: HTMLTextAreaElement): void {
  if (field && mention.field && mention.field !== field) return;
  mention.field?.setAttribute("aria-expanded", "false");
  mention.field?.removeAttribute("aria-activedescendant");
  mention.field = null;
  mention.at = null;
  mention.found = [];
  mentionList.hidden = true;
}

function pickMention(i: number): void {
  const field = mention.field;
  const at = mention.at;
  const person = mention.found[i];
  if (!field || !at || !person) return;
  const put = insertMention(field.value, at.start, at.end, person.login);
  closeMentions();
  field.value = put.text;
  field.setSelectionRange(put.caret, put.caret);
  // As if typed: grows the field and keeps the draft.
  field.dispatchEvent(new Event("input"));
  field.focus();
}

/** Up, Down, Enter, Tab and Escape while suggestions show; true when the key was theirs. */
function mentionKeys(e: KeyboardEvent, field: HTMLTextAreaElement): boolean {
  if (mention.field !== field || !mention.found.length || e.isComposing) return false;
  if (e.key === "ArrowDown" || e.key === "ArrowUp") {
    mention.on = stepHighlight(mention.on, mention.found.length, e.key);
    drawMentions();
  } else if (e.key === "Enter" || e.key === "Tab") {
    pickMention(mention.on);
  } else if (e.key === "Escape") {
    closeMentions();
  } else return false;
  e.preventDefault();
  e.stopPropagation();
  return true;
}

// --- Changing messages: reactions, edits, deletes, pins ---------------------------------

async function react(m: Message, emoji: string, on: boolean): Promise<void> {
  const id = openId();
  if (!id || m.deleted) return;
  // Shown at once; glassd's answer follows.
  const name = myName();
  for (const copy of copies(m.id)) {
    const list = (copy.reactions ?? []).map((r) => ({ ...r, names: [...r.names] }));
    const r = list.find((x) => x.emoji === emoji);
    if (on && !r) list.push({ emoji, count: 1, mine: true, names: [name] });
    else if (on && r && !r.mine) Object.assign(r, { count: r.count + 1, mine: true, names: [...r.names, name] });
    else if (!on && r?.mine) Object.assign(r, { count: r.count - 1, mine: false, names: r.names.filter((n) => n !== name) });
    copy.reactions = list.filter((x) => x.count > 0);
  }
  render();
  await act("Reacting", bridge.asks("chat.react"), () => bridge.call("chat.react", { conversation: id, message: m.id, emoji, on }));
  await refresh();
}

function startEdit(id: string, where: Where): void {
  const m = find(id);
  if (!m || !editable(m)) return;
  closePopover(false);
  state.editing = { id, in: where };
  editField.value = m.text;
  render();
  editField.focus();
  editField.setSelectionRange(editField.value.length, editField.value.length);
  grow(editField, 260);
}

function cancelEdit(): void {
  const where = state.editing?.in;
  state.editing = null;
  closeMentions(editField);
  render();
  (where === "thread" && state.panel?.kind === "thread" ? replyText : text).focus();
}

async function saveEdit(): Promise<void> {
  const editing = state.editing;
  const id = openId();
  const m = editing ? find(editing.id) : undefined;
  if (!editing || !id || !m) return cancelEdit();
  const value = editField.value;
  if (value === m.text) return cancelEdit();
  // An emptied message is one to delete, which asks first.
  if (!value.trim()) return confirmDelete(m);
  for (const copy of copies(m.id)) Object.assign(copy, { text: value, edited: true });
  state.editing = null;
  closeMentions(editField);
  render();
  (editing.in === "thread" && state.panel?.kind === "thread" ? replyText : text).focus();
  await act("Editing", bridge.asks("chat.edit"), () => bridge.call("chat.edit", { conversation: id, message: m.id, text: value }));
  await refresh();
}

async function remove(m: Message): Promise<void> {
  const id = openId();
  if (!id) return;
  if (state.editing?.id === m.id) state.editing = null;
  const done = await act("Deleting", bridge.asks("chat.delete"), () => bridge.call("chat.delete", { conversation: id, message: m.id }));
  if (done !== null) for (const copy of copies(m.id)) Object.assign(copy, { deleted: true, text: "", reactions: [] });
  await refresh();
}

async function pin(m: Message, pinned: boolean): Promise<void> {
  const id = openId();
  if (!id) return;
  for (const copy of copies(m.id)) copy.pinned = pinned;
  if (state.view) {
    const ids = (state.view.pinned ?? []).filter((x) => x !== m.id);
    // Most recently pinned first, as glassd gives them.
    state.view.pinned = pinned ? [m.id, ...ids] : ids;
  }
  render();
  await act(pinned ? "Pinning" : "Unpinning", bridge.asks("chat.pin"), () => bridge.call("chat.pin", { conversation: id, message: m.id, pinned }));
  await refresh();
}

function pinnedIds(): string[] {
  return state.view?.pinned ?? state.messages.filter((m) => m.pinned).map((m) => m.id);
}

// --- The topic and the level -------------------------------------------------------------

function startTopic(): void {
  const c = openRow();
  if (c?.kind !== "channel") return;
  state.topicEditing = true;
  topicField.value = state.view?.topic ?? c.topic ?? "";
  countTopic();
  render();
  topicField.focus();
  topicField.select();
}

function cancelTopic(): void {
  state.topicEditing = false;
  render();
  head.querySelector<HTMLElement>("[data-test=topic]")?.focus();
}

async function saveTopic(): Promise<void> {
  const id = openId();
  const c = openRow();
  if (!state.topicEditing || !id || !c) return;
  const topic = topicField.value.trim().slice(0, TOPIC_MAX);
  state.topicEditing = false;
  const was = state.view?.topic ?? c.topic ?? "";
  if (topic === was) return render();
  if (state.view) state.view.topic = topic || null;
  c.topic = topic || null;
  render();
  await act("Changing the topic", bridge.asks("chat.topic"), () => bridge.call("chat.topic", { conversation: id, topic }));
  await refresh();
}

async function setLevel(level: Level): Promise<void> {
  const id = openId();
  const c = openRow();
  closePopover();
  if (!id || !c) return;
  const done = await act("Changing notifications", bridge.asks("chat.notify"), () => bridge.call("chat.notify", { conversation: id, level }));
  if (done === null) return;
  c.notify = level;
  if (state.view) state.view.notify = level;
  sayDone(`${c.title}: ${LEVELS.find((l) => l.level === level)?.name ?? level}.`);
  render();
}

function level(): Level {
  return state.view?.notify ?? openRow()?.notify ?? "all";
}

// --- Popovers and the delete dialog ---------------------------------------------------------

let popover: { el: HTMLElement; anchor: string | null } | null = null;
let dialog: HTMLElement | null = null;

/** Puts `el` next to `anchor`: below it, or above when there's no room, inside the window. */
function place(el: HTMLElement, anchor: DOMRect, align: "start" | "end" = "end"): void {
  el.style.visibility = "hidden";
  layer.append(el);
  const width = el.offsetWidth;
  const height = el.offsetHeight;
  const below = anchor.bottom + 6 + height <= window.innerHeight - 8;
  const top = below ? anchor.bottom + 6 : Math.max(8, anchor.top - 6 - height);
  const left = align === "end" ? anchor.right - width : anchor.left;
  el.style.top = `${top}px`;
  el.style.left = `${Math.max(8, Math.min(left, window.innerWidth - width - 8))}px`;
  el.style.visibility = "";
}

/**
 * Marks what a popover hangs from, in place rather than by a redraw (which
 * would swap the element under a click): its message keeps its toolbar,
 * and a menu button says it's open. Redraws work it out from `popover`.
 */
function markOpen(key: string | null, open: boolean): HTMLElement | null {
  const anchor = key ? document.querySelector<HTMLElement>(`[data-focus="${CSS.escape(key)}"]`) : null;
  anchor?.closest(".message")?.classList.toggle("tools-open", open);
  if (anchor?.hasAttribute("aria-haspopup")) anchor.setAttribute("aria-expanded", open ? "true" : "false");
  return anchor;
}

function openPopover(el: HTMLElement, anchor: HTMLElement, align: "start" | "end" = "end"): void {
  const key = anchor.dataset.focus ?? null;
  closePopover(false);
  // A redraw since may have drawn the anchor anew.
  const now = (anchor.isConnected ? anchor : markOpen(key, false)) ?? anchor;
  popover = { el, anchor: key };
  place(el, now.getBoundingClientRect(), align);
  markOpen(key, true);
  (el.querySelector<HTMLElement>("[data-first]") ?? el.querySelector<HTMLElement>("button"))?.focus();
}

function closePopover(returnFocus = true): void {
  if (!popover) return;
  const key = popover.anchor;
  popover.el.remove();
  popover = null;
  const anchor = markOpen(key, false);
  if (returnFocus) anchor?.focus();
}

/** Arrow keys move between a popover's buttons, `columns` to a row. */
function arrows(e: KeyboardEvent, el: HTMLElement, columns: number): void {
  const buttons = [...el.querySelectorAll<HTMLElement>("button")];
  const at = buttons.indexOf(document.activeElement as HTMLElement);
  const step = { ArrowRight: 1, ArrowLeft: -1, ArrowDown: columns, ArrowUp: -columns }[e.key];
  if (step === undefined || at < 0) return;
  e.preventDefault();
  buttons[Math.max(0, Math.min(buttons.length - 1, at + step))]?.focus();
}

/** The reaction picker: eight to start with, and about forty more behind "+". */
function openPicker(m: Message, anchor: HTMLElement, more = false): void {
  const choose = (emoji: string) => {
    const mine = m.reactions?.find((r) => r.emoji === emoji)?.mine ?? false;
    closePopover();
    void react(m, emoji, !mine);
  };
  const button = ([emoji, name]: readonly [string, string], first = false) =>
    h(
      "button",
      {
        class: `emoji${m.reactions?.some((r) => r.emoji === emoji && r.mine) ? " mine" : ""}`,
        type: "button",
        title: name,
        "aria-label": name,
        "data-emoji": emoji,
        "data-first": first,
        onclick: () => choose(emoji),
      },
      emoji,
    );
  const el = h(
    "div",
    {
      class: "popover emoji-picker",
      role: "dialog",
      "aria-label": "Add a reaction",
      "data-test": "emoji-picker",
      onkeydown: (e: Event) => arrows(e as KeyboardEvent, el, 9),
    },
    h(
      "div",
      { class: "emoji-row" },
      ...QUICK_EMOJI.map((e, i) => button(e, i === 0)),
      h(
        "button",
        {
          class: `emoji more${more ? " on" : ""}`,
          type: "button",
          title: more ? "Fewer reactions" : "More reactions",
          "aria-label": more ? "Fewer reactions" : "More reactions",
          "aria-expanded": more ? "true" : "false",
          "data-test": "more-emoji",
          onclick: () => {
            openPicker(m, anchor, !more);
            popover?.el.querySelector<HTMLElement>(more ? "[data-test=more-emoji]" : ".emoji-grid button")?.focus();
          },
        },
        icon(more ? "close" : "plus"),
      ),
    ),
    more ? h("div", { class: "emoji-grid" }, ...MORE_EMOJI.map((e) => button(e))) : null,
  );
  openPopover(el, anchor, "end");
}

function openLevels(anchor: HTMLElement): void {
  const now = level();
  const el = h(
    "div",
    {
      class: "popover menu",
      role: "menu",
      "aria-label": "Notifications for this conversation",
      "data-test": "notify-menu",
      onkeydown: (e: Event) => {
        const key = e as KeyboardEvent;
        if (key.key === "ArrowDown" || key.key === "ArrowUp") arrows(key, el, 1);
      },
    },
    h("div", { class: "popover-title" }, "Notify me about"),
    ...LEVELS.map((l) =>
      h(
        "button",
        {
          class: "menu-item",
          type: "button",
          role: "menuitemradio",
          "aria-checked": l.level === now ? "true" : "false",
          "data-level": l.level,
          "data-first": l.level === now,
          onclick: () => void setLevel(l.level),
        },
        h("span", { class: "menu-check", "aria-hidden": "true" }, l.level === now ? icon("check") : null),
        h("span", { class: "menu-text" }, h("span", { class: "menu-name" }, l.name), h("span", { class: "menu-about" }, l.about)),
      ),
    ),
  );
  openPopover(el, anchor, "end");
}

/** Who is in the conversation, and "Add people" for a channel or group. */
function openMembers(anchor: HTMLElement): void {
  const c = openRow();
  if (!c) return;
  const people = [...c.people].sort((a, b) => (a.login === state.me ? -1 : b.login === state.me ? 1 : a.name.localeCompare(b.name)));
  const el = h(
    "div",
    {
      class: "popover members",
      role: "dialog",
      "aria-label": `Members of ${c.title}`,
      "data-test": "members",
      onkeydown: (e: Event) => {
        const key = e as KeyboardEvent;
        if (key.key === "ArrowDown" || key.key === "ArrowUp") arrows(key, el, 1);
      },
    },
    h("div", { class: "popover-title" }, `${people.length} ${people.length === 1 ? "member" : "members"}`),
    h(
      "div",
      { class: "member-list" },
      ...people.map((p) =>
        h(
          "div",
          { class: "member", "data-member": p.login },
          h("span", { class: "avatar small" }, initials(p.name)),
          h("span", { class: "member-name" }, p.name),
          p.login === state.me ? h("span", { class: "muted" }, "you") : h("span", { class: "muted" }, `@${p.login}`),
        ),
      ),
    ),
    c.kind === "direct"
      ? null
      : h(
          "button",
          { class: "menu-item add-people", type: "button", "data-first": true, "data-test": "add-people-button", onclick: () => startAdding() },
          h("span", { class: "menu-check", "aria-hidden": "true" }, icon("plus")),
          h("span", { class: "menu-text" }, h("span", { class: "menu-name" }, "Add people"), h("span", { class: "menu-about" }, "They'll see everything here so far")),
        ),
  );
  openPopover(el, anchor, "end");
}

function startAdding(): void {
  const c = openRow();
  if (!c || c.kind === "direct") return;
  closePopover(false);
  state.adding = { to: { ...picker(), exclude: c.people.map((p) => p.login) }, busy: false, problem: null };
  render();
  document.querySelector<HTMLInputElement>("[name=add]")?.focus();
}

function stopAdding(): void {
  state.adding = null;
  render();
  head.querySelector<HTMLElement>("[data-test=members-button]")?.focus();
}

async function addPeople(): Promise<void> {
  const id = openId();
  const adding = state.adding;
  if (!id || !adding || !adding.to.picked.length || adding.busy) return;
  adding.busy = true;
  adding.problem = null;
  render();
  try {
    const result = await bridge.call<{ conversation: string; added: string[] }>("chat.members", {
      conversation: id,
      add: adding.to.picked.map((p) => p.login),
    });
    if (state.adding !== adding) return;
    if (!result.ok) {
      // As glassd says it: who hasn't opened Chat yet, or why not.
      adding.problem = result.text;
      adding.busy = false;
      return render();
    }
    state.adding = null;
    sayDone(result.text);
    await loadList();
    await loadMessages();
    render();
    text.focus();
  } catch (error) {
    adding.problem = failure(error, "Adding people");
    adding.busy = false;
    render();
  }
}

/** The sheet for adding people, over the page. */
const addBackdrop: HTMLElement = h("div", {
  class: "sheet-backdrop",
  onmousedown: (e: Event) => {
    if (e.target === addBackdrop) stopAdding();
  },
});

function drawAdding(): void {
  const adding = state.adding;
  const c = openRow();
  if (!adding || !c) {
    addBackdrop.remove();
    return;
  }
  const asks = bridge.asks("chat.members");
  show(
    addBackdrop,
    h(
      "form",
      {
        class: "sheet add-sheet",
        role: "dialog",
        "aria-modal": "true",
        "aria-labelledby": "add-title",
        "data-test": "add-people",
        onsubmit: (e: Event) => {
          e.preventDefault();
          void addPeople();
        },
      },
      h("h2", { id: "add-title" }, `Add people to ${c.title}`),
      h("p", { class: "muted" }, "They'll be able to read everything in it so far, and what comes next."),
      pickerView(adding.to, "People", "add"),
      adding.problem ? h("div", { class: "banner refused", role: "status" }, adding.problem) : null,
      h(
        "div",
        { class: "sheet-actions" },
        asks ? h("span", { class: "muted sheet-note" }, "Adding people waits for your approval on the desktop.") : null,
        h("button", { class: "button", type: "button", onclick: stopAdding }, "Cancel"),
        h(
          "button",
          { class: "button primary", type: "submit", "data-test": "add-submit", disabled: !adding.to.picked.length || adding.busy },
          adding.busy ? (asks ? "Waiting for approval…" : "Adding…") : "Add",
        ),
      ),
    ),
  );
  if (!addBackdrop.isConnected) layer.append(addBackdrop);
}

/** Deleting can't be undone, so the page asks first. */
function confirmDelete(m: Message): void {
  closePopover(false);
  const back = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  const close = () => {
    backdrop.remove();
    dialog = null;
    if (back?.isConnected) back.focus();
    else text.focus();
  };
  const cancel = h("button", { class: "button", type: "button", "data-first": true, onclick: close }, "Cancel");
  const confirm = h(
    "button",
    {
      class: "button danger",
      type: "button",
      "data-test": "confirm-delete",
      onclick: () => {
        close();
        void remove(m);
      },
    },
    "Delete",
  );
  const sheet = h(
    "div",
    {
      class: "sheet",
      role: "alertdialog",
      "aria-modal": "true",
      "aria-labelledby": "delete-title",
      "aria-describedby": "delete-about",
      "data-test": "delete-dialog",
      onkeydown: (e: Event) => {
        const key = e as KeyboardEvent;
        if (key.key === "Tab") {
          // Focus stays in the dialog.
          e.preventDefault();
          (document.activeElement === cancel ? confirm : cancel).focus();
        }
      },
    },
    h("h2", { id: "delete-title" }, "Delete this message?"),
    h(
      "p",
      { class: "muted", id: "delete-about" },
      "It's removed for everyone in the conversation, and it can't be undone.",
      state.fingerprint ? " Your organization keeps a sealed copy for compliance." : "",
    ),
    h("blockquote", { class: "quote" }, h("span", { class: "sender" }, m.sender_name), h("span", { class: "quote-text" }, m.text)),
    h("div", { class: "sheet-actions" }, cancel, confirm),
  );
  const backdrop = h(
    "div",
    {
      class: "sheet-backdrop",
      onmousedown: (e: Event) => {
        if (e.target === backdrop) close();
      },
    },
    sheet,
  );
  dialog = backdrop;
  (dialog as HTMLElement & { close?: () => void }).close = close;
  layer.append(backdrop);
  cancel.focus();
}

// --- Live updates ---------------------------------------------------------------------------

/** Waits for news and reads it: one `chat.list`, and the open conversation (and thread), per change. */
async function live(): Promise<void> {
  for (;;) {
    try {
      const status = await bridge.status({ waitSecs: 25, since: state.cursor });
      state.cursor = status.cursor;
      if (status.changed.includes("chat") && !state.sending) {
        await loadList();
        await loadMessages();
        await loadThread();
        render();
      }
      if (state.offline) {
        state.offline = false;
        render();
      }
    } catch {
      state.offline = true;
      render();
      await new Promise((resolve) => setTimeout(resolve, 3000));
    }
  }
}

// --- Drawing ----------------------------------------------------------------------------------

function time(at: string): string {
  const d = new Date(at);
  return Number.isNaN(d.getTime()) ? "" : d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
}

function when(at: string): string {
  const d = new Date(at);
  if (Number.isNaN(d.getTime())) return "";
  const today = new Date();
  return d.toDateString() === today.toDateString() ? time(at) : `${d.toLocaleDateString([], { month: "short", day: "numeric" })}, ${time(at)}`;
}

function row(c: Conversation): HTMLElement {
  const active = openId() === c.id;
  const b = rowBadges(c);
  const draft = !active && hasDraft(c);
  return h(
    "button",
    {
      class: `row${active ? " active" : ""}${b.bold ? " unread" : ""}${b.muted ? " muted" : ""}`,
      "data-conversation": c.id,
      "aria-current": active ? "true" : null,
      onclick: () => void open(c.id),
    },
    h("span", { class: "row-mark" }, c.kind === "channel" ? "#" : c.kind === "group" ? String(c.people.length - 1) : initials(c.title)),
    h("span", { class: "row-title" }, c.kind === "channel" ? c.title.replace(/^#/, "") : c.title),
    draft ? h("span", { class: "row-draft", "data-test": "draft", title: "You have a draft here" }, icon("pencil"), "Draft") : null,
    b.muted ? h("span", { class: "row-muted", title: "Muted", "aria-label": "Muted" }, icon("bellOff")) : null,
    b.mentions
      ? h("span", { class: "badge mention", "data-test": "mention-badge", "aria-label": `${b.mentions} ${b.mentions === 1 ? "mention" : "mentions"}` }, h("span", { class: "badge-at" }, "@"), String(b.mentions))
      : b.count
        ? h("span", { class: "badge", "data-test": "unread-badge", "aria-label": `${b.count} unread` }, String(b.count))
        : null,
  );
}

function renderSidebar(): void {
  const channels = state.conversations.filter((c) => c.kind === "channel");
  const people = state.conversations.filter((c) => c.kind !== "channel");
  show(
    sideLists,
    h(
      "section",
      { class: "side-section" },
      h(
        "h2",
        {},
        "Channels",
        h(
          "button",
          {
            class: "link",
            title: "New channel",
            "aria-label": "New channel",
            onclick: () => {
              flushDraft(openId());
              state.mode = { kind: "new-channel", name: "", to: picker() };
              state.panel = null;
              render();
              document.querySelector<HTMLInputElement>("[name=channel]")?.focus();
            },
          },
          "+",
        ),
      ),
      ...(channels.length ? channels.map(row) : [h("div", { class: "side-empty" }, "No channels yet")]),
    ),
    h(
      "section",
      { class: "side-section" },
      h("h2", {}, "Direct messages"),
      ...(people.length ? people.map(row) : [h("div", { class: "side-empty" }, "No messages yet")]),
    ),
    h(
      "footer",
      { class: "about", "data-test": "about" },
      "🔒 Sealed end to end between your organization's computers.",
      state.fingerprint ? ` IT can read this organization's chat for compliance. Key ${state.fingerprint}.` : "",
    ),
  );
}

function pickerView(p: Picker, label: string, name: string): HTMLElement {
  const input = h("input", {
    name,
    autocomplete: "off",
    placeholder: p.picked.length ? "Add someone" : "Type a name",
    "aria-label": label,
    oninput: (e: Event) => void search(p, (e.target as HTMLInputElement).value),
  });
  input.value = p.query;
  return h(
    "div",
    { class: "picker" },
    h(
      "div",
      { class: "picker-field" },
      h("span", { class: "muted" }, `${label}:`),
      ...p.picked.map((person) =>
        h(
          "span",
          { class: "chip" },
          person.name,
          h(
            "button",
            {
              class: "chip-x",
              "aria-label": `Remove ${person.name}`,
              onclick: () => {
                p.picked = p.picked.filter((x) => x.login !== person.login);
                render();
              },
            },
            "×",
          ),
        ),
      ),
      input,
    ),
    p.found.length
      ? h(
          "div",
          { class: "suggestions", role: "listbox" },
          ...p.found.map((person) =>
            h(
              "button",
              {
                class: "suggestion",
                role: "option",
                "data-login": person.login,
                onclick: () => {
                  p.picked.push(person);
                  p.query = "";
                  p.found = [];
                  render();
                  document.querySelector<HTMLInputElement>(`[name=${name}]`)?.focus();
                },
              },
              h("span", { class: "avatar small" }, initials(person.name)),
              h("span", {}, person.name),
              person.title ? h("span", { class: "muted" }, ` · ${person.title}`) : null,
            ),
          ),
        )
      : null,
  );
}

/**
 * The Artifacts app's page for item `item`: this host with its leading
 * `<app>.` swapped for `artifacts.`, the same port, and the item in the
 * hash. Null for anything that isn't an artifact's id.
 */
function artifactsUrl(item: string): string | null {
  const app = /^[a-z][a-z0-9-]*\./;
  if (!/^cv-[0-9a-f]{32}$/.test(item) || !app.test(location.host)) return null;
  return `${location.protocol}//${location.host.replace(app, "artifacts.")}/#${item}`;
}

/** An artifact shared here (AR-5): a card that opens it in Artifacts, never a file. */
function artifactCard(shared: Shared): HTMLElement {
  const url = artifactsUrl(shared.item);
  return h(
    "div",
    { class: "card", "data-test": "artifact-card", "data-item": shared.item },
    h("span", { class: "card-icon", "aria-hidden": "true" }, h("span", { class: "card-page" })),
    h("span", { class: "card-body" }, h("span", { class: "card-title" }, shared.title), h("span", { class: "card-kind" }, "Artifact · opens in Artifacts")),
    url ? h("a", { class: "button card-open", href: url, target: "_blank", rel: "noopener" }, "Open") : null,
  );
}

/** Text with @mentions as chips; the person's own stands out. Text nodes only. */
function richText(m: Message): (Node | string)[] {
  return mentionSegments(m.text, m.mentions).map((s) =>
    "mention" in s ? h("span", { class: `mention${s.mention.login === state.me ? " me" : ""}`, title: `@${s.mention.login}`, "data-mention-chip": s.mention.login }, `@${s.mention.name}`) : s.text,
  );
}

function tool(act: string, label: string, name: keyof typeof ICONS, focus: string, run: (button: HTMLElement) => void): HTMLElement {
  const button: HTMLElement = h(
    "button",
    {
      class: "tool",
      type: "button",
      title: label,
      "aria-label": label,
      "data-act": act,
      "data-focus": `${focus}:${act}`,
      onclick: () => run(button),
    },
    icon(name),
  );
  return button;
}

function messageView(m: Message, previous: Message | undefined, where: Where): HTMLElement {
  const by = parseAttribution(m.attribution);
  const agent = showAttribution(by, "chat");
  const joined = joins(previous, m);
  const key = `${where}:${m.id}`;
  const editing = state.editing?.id === m.id && state.editing.in === where;
  const isRoot = where === "timeline";
  const summary = isRoot ? threadSummary(m, new Date()) : null;
  const pills = m.deleted ? [] : reactionPills(m.reactions, myName());
  const roving = state.focused[where] ?? null;
  const classes = [
    "message",
    joined ? "joined" : "",
    m.pinned && !m.deleted ? "pinned" : "",
    mentionsMe(m, state.me) ? "mentioned" : "",
    state.highlight === m.id ? "highlight" : "",
    m.deleted ? "deleted" : "",
    editing ? "editing" : "",
    popover?.anchor?.startsWith(`${key}:`) ? "tools-open" : "",
  ].filter(Boolean);
  const el: HTMLElement = h(
    "div",
    {
      class: classes.join(" "),
      "data-message": m.id,
      "data-focus": key,
      tabindex: roving === m.id ? 0 : -1,
      "aria-label": `${m.mine ? "You" : m.sender_name}, ${when(m.at)}`,
      onfocus: (e: Event) => {
        if (e.target === el) state.focused[where] = m.id;
      },
      onkeydown: (e: Event) => messageKeys(e as KeyboardEvent, el, m, where),
    },
    m.pinned && !m.deleted ? h("div", { class: "pin-note" }, icon("pin"), "Pinned") : null,
    joined ? h("span", { class: "avatar-space" }, h("span", { class: "hover-time" }, time(m.at))) : h("span", { class: "avatar" }, initials(m.sender_name)),
    h(
      "div",
      { class: "message-body" },
      joined
        ? null
        : h(
            "div",
            { class: "message-head" },
            h("span", { class: "sender" }, m.mine ? `${m.sender_name} (you)` : m.sender_name),
            h("span", { class: "time", title: when(m.at) }, time(m.at)),
            agent && by ? attributionChip(by.text) : null,
          ),
      m.deleted
        ? h("div", { class: "message-text deleted-text" }, "This message was deleted.")
        : editing
          ? h(
              "div",
              { class: "edit" },
              editField,
              h(
                "div",
                { class: "edit-actions" },
                h("span", { class: "muted" }, "Enter to save · Esc to cancel"),
                h("button", { class: "button small", type: "button", onclick: () => cancelEdit() }, "Cancel"),
                h("button", { class: "button primary small", type: "button", "data-test": "save-edit", onclick: () => void saveEdit() }, "Save"),
              ),
            )
          : m.shared?.kind === "artifact"
            ? artifactCard(m.shared)
            : h("div", { class: "message-text" }, ...richText(m), m.edited ? h("span", { class: "edited", title: "Edited" }, " (edited)") : null),
      pills.length
        ? h(
            "div",
            { class: "reactions", "data-test": "reactions" },
            ...pills.map((p) =>
              h(
                "button",
                {
                  class: `reaction${p.mine ? " mine" : ""}`,
                  type: "button",
                  "aria-label": p.label,
                  "aria-pressed": p.mine ? "true" : "false",
                  "data-emoji": p.emoji,
                  "data-focus": `${key}:pill:${p.emoji}`,
                  onclick: () => void react(m, p.emoji, !p.mine),
                },
                h("span", { class: "reaction-emoji" }, p.emoji),
                h("span", { class: "reaction-count" }, String(p.count)),
                h("span", { class: "tip", "aria-hidden": "true" }, p.title),
              ),
            ),
            (() => {
              const add: HTMLElement = h(
                "button",
                {
                  class: "reaction add",
                  type: "button",
                  title: "Add reaction",
                  "aria-label": "Add reaction",
                  "data-focus": `${key}:add`,
                  onclick: () => openPicker(m, add),
                },
                icon("smile"),
                h("span", { class: "reaction-plus", "aria-hidden": "true" }, "+"),
              );
              return add;
            })(),
          )
        : null,
      summary
        ? h(
            "button",
            {
              class: "thread-summary",
              type: "button",
              "data-test": "thread-summary",
              "data-focus": `${key}:summary`,
              "aria-label": `${summary.text}. Open the thread`,
              onclick: () => void openThread(m.id),
            },
            ...summary.repliers.map((n) => h("span", { class: "avatar tiny", title: n }, initials(n))),
            h("span", { class: "thread-count" }, summary.count),
            summary.last ? h("span", { class: "thread-last" }, `· ${summary.last}`) : null,
            h("span", { class: "thread-open" }, "View thread"),
          )
        : null,
    ),
    m.deleted || editing
      ? null
      : h(
          "div",
          { class: "toolbar", role: "toolbar", "aria-label": "Message actions" },
          ...QUICK_EMOJI.slice(0, 3).map(([emoji, name]) =>
            h(
              "button",
              {
                class: "tool quick",
                type: "button",
                title: `React with ${name}`,
                "aria-label": `React with ${name}`,
                "data-act": `quick-${name.replace(/\s+/g, "-")}`,
                "data-focus": `${key}:quick:${emoji}`,
                onclick: () => {
                  const mine = m.reactions?.find((r) => r.emoji === emoji)?.mine ?? false;
                  void react(m, emoji, !mine);
                },
              },
              emoji,
            ),
          ),
          tool("react", "Add reaction", "smile", key, (b) => openPicker(m, b)),
          isRoot ? tool("reply", "Reply in thread", "thread", key, () => void openThread(m.id)) : null,
          tool("pin", m.pinned ? "Unpin" : "Pin to this conversation", "pin", key, () => void pin(m, !m.pinned)),
          editable(m) ? tool("edit", "Edit", "pencil", key, () => startEdit(m.id, where)) : null,
          m.mine ? tool("delete", "Delete", "trash", key, () => confirmDelete(m)) : null,
        ),
  );
  return el;
}

/** On a message: arrows move, and a letter does what its toolbar button does (UX-20). */
function messageKeys(e: KeyboardEvent, el: HTMLElement, m: Message, where: Where): void {
  if (e.target !== el || e.altKey || e.ctrlKey || e.metaKey) return;
  const list = [...(el.parentElement?.querySelectorAll<HTMLElement>(":scope > .message[data-message]") ?? [])];
  const at = list.indexOf(el);
  const go = (i: number) => {
    const next = list[Math.max(0, Math.min(list.length - 1, i))];
    if (!next) return;
    for (const x of list) x.tabIndex = -1;
    next.tabIndex = 0;
    next.focus();
    state.focused[where] = next.dataset.message ?? null;
  };
  const press = (act: string) => el.querySelector<HTMLElement>(`:scope > .toolbar [data-act=${act}]`)?.click();
  switch (e.key) {
    case "ArrowUp":
      go(at - 1);
      break;
    case "ArrowDown":
      go(at + 1);
      break;
    case "Home":
      go(0);
      break;
    case "End":
      go(list.length - 1);
      break;
    case "Enter":
    case "t":
      if (where === "timeline" && (!m.deleted || (m.replies ?? 0) > 0)) void openThread(m.id);
      break;
    case "r":
      press("react");
      break;
    case "p":
      press("pin");
      break;
    case "e":
      press("edit");
      break;
    case "Delete":
    case "Backspace":
      press("delete");
      break;
    default:
      return;
  }
  e.preventDefault();
}

function divider(kind: "new" | "day" | "replies", label: string): HTMLElement {
  return h("div", { class: `divider ${kind}`, role: "separator", "aria-label": label, "data-test": kind === "new" ? "new-messages" : null }, h("span", {}, label));
}

function pending(): HTMLElement | null {
  const sending = state.sending;
  if (!sending) return null;
  return h(
    "div",
    { class: "message pending", "data-test": "sending" },
    h("span", { class: "avatar" }, "…"),
    h("div", { class: "message-body" }, h("div", { class: "message-head" }, h("span", { class: "sender" }, sending.waiting ?? "Sending…")), h("div", { class: "message-text" }, sending.text)),
  );
}

/** Keeps a list where the person had it, at the newest, or at a message to point at. */
function scroll(list: HTMLElement, before: { top: number; bottom: boolean } | null, stick: boolean): void {
  const at = state.scrollTo ? list.querySelector<HTMLElement>(`[data-message="${CSS.escape(state.scrollTo)}"]`) : null;
  if (at) {
    // A third of the way down: "New messages" or what came before stays in sight.
    list.scrollTop = at.offsetTop - list.clientHeight / 3;
    state.scrollTo = null;
  } else if (!before || before.bottom || stick) list.scrollTop = list.scrollHeight;
  else list.scrollTop = before.top;
}

function where(list: Element | null): { top: number; bottom: boolean } | null {
  if (!(list instanceof HTMLElement)) return null;
  return { top: list.scrollTop, bottom: list.scrollHeight - list.scrollTop - list.clientHeight < 48 };
}

function drawTimeline(): void {
  const before = where(body.querySelector(".messages"));
  const now = new Date();
  if (state.focused.timeline && !state.messages.some((m) => m.id === state.focused.timeline)) state.focused.timeline = null;
  if (!state.focused.timeline) state.focused.timeline = state.messages[state.messages.length - 1]?.id ?? null;
  const items: Node[] = [];
  if (state.moreBefore)
    items.push(
      h(
        "button",
        {
          class: "button small earlier",
          type: "button",
          onclick: async () => {
            const first = state.messages[0]?.id ?? null;
            if (await loadEarlier()) {
              state.scrollTo = first;
              render();
            }
          },
        },
        "Show earlier messages",
      ),
    );
  state.messages.forEach((m, i) => {
    const previous = state.messages[i - 1];
    if (newDay(previous, m)) items.push(divider("day", dayLabel(m.at, now)));
    const first = state.divider === m.id;
    if (first) items.push(divider("new", "New messages"));
    items.push(messageView(m, first ? undefined : previous, "timeline"));
  });
  const list = h(
    "div",
    { class: "messages", "data-test": "messages", "aria-label": "Messages" },
    ...(state.messages.length ? items : [h("div", { class: "empty" }, state.view ? "No messages yet. Say hello." : "")]),
    state.sending && !state.sending.thread ? pending() : null,
  );
  show(body, list);
  scroll(list, before, state.stick);
  state.stick = false;
}

function drawSearch(mode: Extract<Mode, { kind: "search" }>): void {
  const groups = mode.hits ? groupHits(mode.hits) : [];
  show(
    body,
    h(
      "div",
      { class: "search-results", "data-test": "search-results" },
      mode.hits === null
        ? h("div", { class: "empty" }, "Searching…")
        : !mode.hits.length
          ? h("div", { class: "empty" }, h("h2", {}, "Nothing found"), h("p", { class: "muted" }, `No messages match “${mode.query}”. Try other words.`))
          : null,
      ...groups.map((g) =>
        h(
          "section",
          { class: "hit-group", "data-conversation": g.conversation },
          h("h3", {}, g.title, h("span", { class: "muted" }, ` · ${g.hits.length} ${g.hits.length === 1 ? "message" : "messages"}`)),
          ...g.hits.map((hit) =>
            h(
              "button",
              {
                class: "hit",
                type: "button",
                "data-hit": hit.message,
                onclick: () => void jumpTo(hit.conversation, hit.message, hit.thread),
                onkeydown: (e: Event) => {
                  const key = e as KeyboardEvent;
                  if (key.key !== "ArrowDown" && key.key !== "ArrowUp") return;
                  e.preventDefault();
                  const all = [...body.querySelectorAll<HTMLElement>("[data-hit]")];
                  const at = all.indexOf(e.currentTarget as HTMLElement);
                  (all[at + (key.key === "ArrowDown" ? 1 : -1)] ?? (key.key === "ArrowUp" ? searchField : null))?.focus();
                },
              },
              h("span", { class: "avatar small" }, initials(hit.sender_name)),
              h(
                "span",
                { class: "hit-body" },
                h("span", { class: "hit-head" }, h("span", { class: "sender" }, hit.sender_name), h("span", { class: "time" }, when(hit.at)), hit.thread ? h("span", { class: "hit-thread" }, "in a thread") : null),
                h("span", { class: "hit-text" }, ...matchSegments(hit.text, mode.query).map((s) => (s.match ? h("mark", {}, s.text) : s.text))),
              ),
            ),
          ),
        ),
      ),
    ),
  );
}

function drawBody(): void {
  const mode = state.mode;
  if (mode.kind === "new-message") {
    show(body, pickerView(mode.to, "To", "to"), h("div", { class: "empty" }, mode.to.picked.length ? "Write your message below." : "Who is it for?"));
  } else if (mode.kind === "new-channel") {
    const name = h("input", {
      name: "channel",
      placeholder: "northwind-deal",
      "aria-label": "Channel name",
      oninput: (e: Event) => {
        mode.name = (e.target as HTMLInputElement).value;
      },
    });
    name.value = mode.name;
    show(
      body,
      h(
        "form",
        {
          class: "new-channel",
          onsubmit: (e: Event) => {
            e.preventDefault();
            if (mode.name.trim() && mode.to.picked.length) void createChannel(mode.name.trim(), mode.to.picked);
          },
        },
        h("label", { class: "field" }, h("span", { class: "muted" }, "Name"), h("span", { class: "hash" }, "#", name)),
        pickerView(mode.to, "People", "people"),
        h(
          "div",
          { class: "actions" },
          h("button", { class: "button primary", type: "submit" }, "Make channel"),
          h("span", { class: "muted" }, bridge.asks("chat.create") ? "Making a channel waits for your approval on the desktop." : ""),
        ),
      ),
    );
  } else if (mode.kind === "conversation") {
    drawTimeline();
  } else if (mode.kind === "search") {
    drawSearch(mode);
  } else {
    show(
      body,
      h(
        "div",
        { class: "empty welcome" },
        h("h2", {}, "Chat with people in your organization"),
        h("p", { class: "muted" }, "Start a message, or make a channel for a team or a project. Everything is sealed end to end; agents read it to help you, and post only when you say yes."),
        h(
          "button",
          {
            class: "button primary",
            onclick: () => {
              state.mode = { kind: "new-message", to: picker() };
              render();
            },
          },
          "New message",
        ),
      ),
    );
  }
}

/** The composer shows when there is somewhere to send. */
function renderComposer(): void {
  const mode = state.mode;
  composer.hidden = !(mode.kind === "conversation" || (mode.kind === "new-message" && mode.to.picked.length > 0));
  const c = openRow();
  text.placeholder = c ? `Message ${c.title}` : mode.kind === "new-message" ? `Message ${mode.to.picked.map((p) => p.name).join(", ")}` : "Message";
}

function renderHead(): void {
  const mode = state.mode;
  const c = openRow();
  const title = mode.kind === "new-message" ? "New message" : mode.kind === "new-channel" ? "New channel" : mode.kind === "search" ? "Search" : (c?.title ?? "Chat");
  const topic = state.view?.topic ?? c?.topic ?? "";
  const pins = pinnedIds().length;
  const now = level();
  const tools: (HTMLElement | null)[] = [];
  if (c) {
    if (c.kind !== "direct") {
      const names = c.people.map((p) => (p.login === state.me ? "You" : p.name)).join(", ");
      const others = c.people.filter((p) => p.login !== state.me);
      const members: HTMLElement = h(
        "button",
        {
          class: "head-button members-button",
          type: "button",
          "data-test": "members-button",
          "data-focus": "head:members",
          "aria-haspopup": "dialog",
          "aria-label": `${c.people.length} members: ${names}`,
          title: names,
          onclick: () => openMembers(members),
        },
        h("span", { class: "stack", "aria-hidden": "true" }, ...others.slice(0, 3).map((p) => h("span", { class: "avatar tiny" }, initials(p.name)))),
        h("span", { class: "head-count-plain" }, String(c.people.length)),
      );
      tools.push(members);
    }
    tools.push(
      h(
        "button",
        {
          class: `head-button${state.panel?.kind === "pins" ? " on" : ""}`,
          type: "button",
          "data-test": "pins-button",
          "data-focus": "head:pins",
          "aria-pressed": state.panel?.kind === "pins" ? "true" : "false",
          "aria-label": `Pinned messages: ${pins}`,
          title: "Pinned messages",
          onclick: togglePins,
        },
        icon("pin"),
        h("span", {}, "Pinned"),
        pins ? h("span", { class: "head-count", "data-test": "pins-count" }, String(pins)) : null,
      ),
    );
    const levelName = LEVELS.find((l) => l.level === now)?.name ?? "";
    const bell: HTMLElement = h(
      "button",
      {
        class: `head-button icon-only${now === "none" ? " on" : ""}`,
        type: "button",
        "data-test": "notify-button",
        "data-focus": "head:notify",
        "data-level": now,
        "aria-haspopup": "menu",
        "aria-expanded": popover?.anchor === "head:notify" ? "true" : "false",
        "aria-label": `Notifications: ${levelName}`,
        title: `Notifications: ${levelName}`,
        onclick: () => openLevels(bell),
      },
      icon(now === "none" ? "bellOff" : now === "mentions" ? "at" : "bell"),
    );
    tools.push(bell);
  }
  show(
    head,
    h(
      "div",
      { class: "head-main" },
      h("h1", {}, title),
      mode.kind === "search" ? h("span", { class: "muted people" }, `“${mode.query}”`) : null,
      c?.kind === "channel"
        ? state.topicEditing
          ? h("span", { class: "topic-edit" }, topicField, topicCount)
          : h(
              "button",
              {
                class: `topic${topic ? "" : " empty"}`,
                type: "button",
                "data-test": "topic",
                "data-focus": "head:topic",
                title: topic ? `${topic} · Click to change the topic` : "Add a topic",
                onclick: startTopic,
              },
              topic || "Add a topic",
            )
        : c && c.kind === "group"
          ? h("span", { class: "muted people" }, c.people.map((p) => (p.login === state.me ? "You" : p.name)).join(", "))
          : null,
    ),
    tools.length ? h("div", { class: "head-tools" }, ...tools) : null,
  );
}

function renderPanel(): void {
  const open = state.panel;
  root.classList.toggle("with-panel", open !== null);
  panel.hidden = open === null;
  replyWrap.hidden = open?.kind !== "thread";
  if (!open) return;
  const c = openRow();
  const close = h(
    "button",
    { class: "head-button icon-only", type: "button", title: "Close (Esc)", "aria-label": "Close", "data-act": "close-panel", "data-focus": "panel:close", onclick: closePanel },
    icon("close"),
  );
  const before = where(panelBody.querySelector(".panel-list"));
  if (open.kind === "thread") {
    panel.setAttribute("aria-label", "Thread");
    show(panelHead, h("div", { class: "panel-title" }, h("h2", {}, "Thread"), c ? h("span", { class: "muted" }, c.title) : null), close);
    const [first, ...replies] = open.messages;
    if (state.focused.thread && !open.messages.some((m) => m.id === state.focused.thread)) state.focused.thread = null;
    if (!state.focused.thread) state.focused.thread = open.messages[open.messages.length - 1]?.id ?? null;
    const list = h(
      "div",
      { class: "messages panel-list", "data-test": "thread" },
      first ? messageView(first, undefined, "thread") : h("div", { class: "empty" }, open.loaded ? "This thread can't be found." : "Loading…"),
      first ? divider("replies", replies.length ? `${replies.length} ${replies.length === 1 ? "reply" : "replies"}` : "No replies yet") : null,
      ...replies.map((m, i) => messageView(m, replies[i - 1], "thread")),
      state.sending?.thread === open.root ? pending() : null,
    );
    show(panelBody, list);
    replyText.placeholder = first ? `Reply to ${first.mine ? "your message" : first.sender_name}` : "Reply…";
    scroll(list, before, !before || stickThread);
    stickThread = false;
  } else {
    panel.setAttribute("aria-label", "Pinned messages");
    const ids = pinnedIds();
    show(panelHead, h("div", { class: "panel-title" }, h("h2", {}, "Pinned"), h("span", { class: "muted" }, ids.length ? `${ids.length} in ${c?.title ?? "this conversation"}` : "")), close);
    show(
      panelBody,
      h(
        "div",
        { class: "panel-list pins", "data-test": "pins" },
        ...(ids.length
          ? ids.map((id) => {
              const m = known.get(id);
              const conversation = openId();
              return h(
                "div",
                { class: "pin-item", "data-pin": id },
                h(
                  "button",
                  {
                    class: "pin-open",
                    type: "button",
                    "data-focus": `pin:${id}`,
                    title: "Show it in the conversation",
                    onclick: () => {
                      if (conversation) void jumpTo(conversation, id, m?.thread ?? null);
                    },
                  },
                  ...(m
                    ? [
                        h("span", { class: "pin-head" }, h("span", { class: "sender" }, m.sender_name), h("span", { class: "time" }, when(m.at)), m.thread ? h("span", { class: "hit-thread" }, "in a thread") : null),
                        h("span", { class: "pin-text" }, ...(m.deleted ? ["This message was deleted."] : m.shared ? [m.shared.title] : richText(m))),
                      ]
                    : [h("span", { class: "pin-text muted" }, "A message from earlier in the conversation")]),
                ),
                h(
                  "button",
                  {
                    class: "tool",
                    type: "button",
                    title: "Unpin",
                    "aria-label": "Unpin",
                    "data-act": "unpin",
                    onclick: () => {
                      const target = m ?? ({ id } as Message);
                      void pin(target, false);
                    },
                  },
                  icon("close"),
                ),
              );
            })
          : [h("div", { class: "empty" }, h("h2", {}, "Nothing pinned yet"), h("p", { class: "muted" }, "Pin a message from its menu to keep it here for everyone in the conversation."))]),
      ),
    );
  }
}

function render(): void {
  // Whatever had the focus keeps it, wherever it is drawn now.
  const active = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  const field = active instanceof HTMLInputElement || active instanceof HTMLTextAreaElement ? active : null;
  const selection = field ? { start: field.selectionStart, end: field.selectionEnd } : null;
  const focusKey = active?.dataset.focus ?? null;
  rendering = true;
  try {
    renderSidebar();
    renderHead();
    show(
      banner,
      state.notice && h("div", { class: `banner ${state.notice.tone === "ok" ? "done" : "refused"}`, role: "status" }, state.notice.text),
      state.offline && h("div", { class: "banner offline" }, "Can't reach the system right now. Nothing you write is sent until it's back."),
    );
    drawBody();
    renderComposer();
    renderPanel();
    drawAdding();
    show(hint, bridge.asks("chat.post") ? "Your organization asks you to approve each message on the desktop." : "Enter to send · Shift+Enter for a new line · @ to mention · ↑ to edit your last message");
    if (field && document.activeElement !== field) {
      const again = field.isConnected ? field : field.name ? document.querySelector<HTMLInputElement | HTMLTextAreaElement>(`[name="${field.name}"]`) : null;
      if (again) {
        again.focus({ preventScroll: true });
        if (selection && selection.start !== null && selection.end !== null) {
          try {
            again.setSelectionRange(Math.min(selection.start, again.value.length), Math.min(selection.end, again.value.length));
          } catch {
            // Not every field has a selection.
          }
        }
      }
    } else if (!field && focusKey && active && !active.isConnected) {
      root.querySelector<HTMLElement>(`[data-focus="${CSS.escape(focusKey)}"]`)?.focus({ preventScroll: true });
    }
  } finally {
    rendering = false;
  }
}

/** Escape closes what's on top: the dialog, a popover, an edit, the topic, the panel, then search. */
function escape(e: KeyboardEvent): void {
  if (e.key !== "Escape" || e.defaultPrevented) return;
  if (dialog) (dialog as HTMLElement & { close?: () => void }).close?.();
  else if (state.adding) stopAdding();
  else if (popover) closePopover();
  else if (state.editing) cancelEdit();
  else if (state.topicEditing) cancelTopic();
  else if (state.panel) closePanel();
  else if (state.mode.kind === "search") {
    searchField.value = "";
    leaveSearch();
  } else return;
  e.preventDefault();
}

async function start(): Promise<void> {
  show(
    root,
    h(
      "aside",
      { class: "sidebar" },
      h(
        "div",
        { class: "side-head" },
        h("h1", {}, "Chat"),
        h(
          "button",
          {
            class: "icon-button",
            title: "New message",
            "aria-label": "New message",
            onclick: () => {
              flushDraft(openId());
              state.mode = { kind: "new-message", to: picker() };
              state.panel = null;
              render();
              document.querySelector<HTMLInputElement>("[name=to]")?.focus();
            },
          },
          icon("pencil"),
        ),
      ),
      h("label", { class: "side-search" }, icon("search"), searchField),
      sideLists,
    ),
    h("main", { class: "pane" }, head, banner, body, h("div", { class: "composer-wrap" }, composer, hint)),
    panel,
  );
  layer.append(mentionList);
  document.body.append(layer);
  document.addEventListener("keydown", escape);
  document.addEventListener("mousedown", (e) => {
    if (popover && !popover.el.contains(e.target as Node)) closePopover(false);
  });
  // Leaving the page keeps what's typed as a draft.
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "hidden") flushAll();
  });
  window.addEventListener("pagehide", flushAll);
  try {
    await bridge.open();
  } catch (error) {
    state.notice = { text: failure(error, "Chat"), tone: "bad" };
  }
  await loadList();
  const first = state.conversations[0];
  if (first) await open(first.id);
  else render();
  ready();
  try {
    state.cursor = (await bridge.status()).cursor;
  } catch {
    // live() says so.
  }
  void live();
}

void start();
