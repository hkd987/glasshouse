// Artifacts (AR-1 to AR-5, ADR 0033): pages people and their agents share
// with each other, each shown in a sealed frame on an origin of its own,
// with everyone's comments beside it and pinned on it.
//
// The page is the bridge's to serve, at `<hex>.artifact.localhost`, under a
// CSP sandbox: no network, no storage, no frames, and only this app may
// frame it. This page frames it with `sandbox="allow-scripts"` too, and
// talks to it only through the bridge's pin script: it sends element paths
// and whether pin mode is on, never a comment; it hears clicks and where
// elements are, and checks every field (artifacts.ts). It draws the pins
// itself, so a hostile page can only misplace pins on itself. A page that
// navigates itself away is put back (ADR 0033 §4).
//
// Like Chat, it keeps nothing: every call is `bridge.call`, and news comes
// from `app.status` waits, ids only.

import { attributionChip, Bridge, BridgeError, firstCursor, h, Offline, parseAttribution, ready, show, showAttribution, watch } from "@glasshouse/sdk";

import {
  type Artifact,
  type Comment,
  type Label,
  type Person,
  type Pinned,
  type Rect,
  type Row,
  artifactUrl,
  deepLink,
  initials,
  isHtmlFile,
  labelName,
  MAX_PAGE_BYTES,
  MAX_TEXT_CHARS,
  partition,
  pinned,
  pinPosition,
  readable,
  readFrameMessage,
  SHARE_LABELS,
  shareInput,
  size,
  threads,
  updateInput,
  versionBadge,
  when,
  wherePaths,
} from "./artifacts.ts";

interface Found {
  login: string;
  name: string;
  email?: string;
  title?: string;
}

interface Conversation {
  id: string;
  kind: string;
  title: string;
  people: Person[];
}

interface Open {
  id: string;
  data: Artifact | null;
  /** The version in the frame. */
  shown: number | null;
  /** The person picked a version: a newer one doesn't take its place. */
  chosen: boolean;
}

/** A frame showing one version, and how many times it has loaded. */
interface Frame {
  el: HTMLIFrameElement;
  id: string;
  version: number;
  url: string;
  loads: number;
  ready: boolean;
}

/** How many times a page is put back before it isn't shown at all. */
const MAX_PUT_BACK = 3;

const bridge = new Bridge({ app: "artifacts" });
const state = {
  me: "",
  rows: [] as Row[],
  open: null as Open | null,
  /** Pin mode: the next click in the page picks where a comment goes. */
  pinning: false,
  /** Where the comment being written goes. */
  pick: null as { path: string | null; x: number; y: number } | null,
  replyTo: null as string | null,
  replyDraft: "",
  busy: false,
  notice: null as { text: string; tone: "bad" | "ok" } | null,
  offline: false,
  cursor: 0,
};

let frame: Frame | null = null;
/** Times the shown page was put back after it navigated. */
let putBack = 0;
/** Versions that kept navigating away, so aren't shown again until the app reopens. */
const refused = new Set<string>();
/** What the frame was last asked about, and where it said those are. */
let asked: string[] = [];
const rects = new Map<string, Rect>();

const root = document.getElementById("app") as HTMLElement;
// The parts that redraw. The frame, its pins and the composer stay, so a
// live update never reloads the page or interrupts typing.
const sidebar = h("aside", { class: "sidebar" });
const head = h("header", { class: "pane-head" });
const banners = h("div", { class: "banners" });
const welcome = h("div", { class: "welcome-wrap" });
const frameNotice = h("div", { class: "frame-notice", role: "status", "data-test": "frame-notice", hidden: true });
const pickHint = h("div", { class: "pick-hint", hidden: true }, "Click anything on the page to pin a comment to it. Esc stops.");
const overlay = h("div", { class: "pins", "data-test": "pins" });
const stage = h("div", { class: "stage", "data-test": "stage" });
const versionLine = h("div", { class: "version-line" });
const commentsHead = h("div", { class: "comments-head" });
const list = h("div", { class: "comments-list", "data-test": "comments" });
const anchorChip = h("div", { class: "anchor-chip", hidden: true });
const composerHint = h("span", { class: "composer-hint" });
const composerText = h("textarea", {
  class: "composer-text",
  name: "comment",
  rows: 3,
  maxlength: MAX_TEXT_CHARS,
  placeholder: "Add a comment",
  "aria-label": "Comment",
  onkeydown: (e: Event) => {
    const key = e as KeyboardEvent;
    if (key.key === "Enter" && (key.metaKey || key.ctrlKey)) {
      e.preventDefault();
      void comment();
    }
  },
});
const composerButton = h("button", { class: "button primary", type: "submit" }, "Comment");
const composer = h(
  "form",
  {
    class: "composer",
    "data-test": "composer",
    onsubmit: (e: Event) => {
      e.preventDefault();
      void comment();
    },
  },
  anchorChip,
  composerText,
  h("div", { class: "composer-row" }, composerHint, composerButton),
);
const viewer = h(
  "div",
  { class: "viewer", hidden: true },
  h("div", { class: "stage-wrap" }, versionLine, frameNotice, pickHint, stage),
  h("aside", { class: "comments" }, commentsHead, list, composer),
);
const body = h("div", { class: "pane-body" }, welcome, viewer);

function failure(error: unknown, doing: string): string {
  if (error instanceof Offline) return `${doing} needs the system, and it isn't answering.`;
  if (error instanceof BridgeError) return readable(error.message);
  return readable(String(error));
}

function bad(text: string): { text: string; tone: "bad" } {
  return { text: readable(text), tone: "bad" };
}

// --- The frame -------------------------------------------------------

/** Sends the frame one of the two things it may hear: pin mode, or which paths to locate. */
function tell(f: Frame, message: { type: "pin"; on: boolean } | { type: "where"; paths: string[] }): void {
  // Its origin is no one's (sandboxed), so no target origin would match.
  f.el.contentWindow?.postMessage({ glass: "artifact", ...message }, "*");
}

/** Shows version `version` of artifact `id`, unless it is shown already. */
function mountFrame(id: string, version: number): void {
  if (frame && frame.id === id && frame.version === version) return;
  if (refused.has(`${id}/v${version}`)) {
    if (frame) unmountFrame();
    show(frameNotice, "This page keeps trying to open other pages, so it isn't shown.");
    frameNotice.hidden = false;
    return;
  }
  const url = artifactUrl(location, id, version);
  putBack = 0;
  frameNotice.hidden = true;
  if (!url) {
    unmountFrame();
    show(stage, h("div", { class: "empty" }, "This artifact can't be shown at this address."));
    return;
  }
  loadFrame(id, version, url);
}

function loadFrame(id: string, version: number, url: string): void {
  const el = h("iframe", {
    class: "frame",
    sandbox: "allow-scripts",
    referrerpolicy: "no-referrer",
    title: "The artifact's page",
    "data-test": "frame",
  });
  const f: Frame = { el, id, version, url, loads: 0, ready: false };
  el.addEventListener("load", () => loaded(f));
  el.src = url;
  frame = f;
  asked = [];
  rects.clear();
  stage.dataset.ready = "false";
  show(stage, el, overlay);
  show(overlay);
}

function unmountFrame(): void {
  frame = null;
  asked = [];
  rects.clear();
  show(stage);
}

/**
 * Its first load is the artifact. Any later one means the page navigated
 * itself away (a link, `location`), maybe carrying what it holds: it is
 * put back in a new frame, and the person told.
 */
function loaded(f: Frame): void {
  if (frame !== f) return;
  f.loads += 1;
  if (f.loads === 1) return;
  putBack += 1;
  setPinning(false);
  show(frameNotice, "This page tried to open another page; it stays here.");
  frameNotice.hidden = false;
  if (putBack > MAX_PUT_BACK) {
    refused.add(`${f.id}/v${f.version}`);
    unmountFrame();
    show(frameNotice, "This page keeps trying to open other pages, so it isn't shown.");
    return;
  }
  loadFrame(f.id, f.version, f.url);
}

window.addEventListener("message", (event: MessageEvent) => {
  const f = frame;
  // Only the frame shown now, and only the pin script's messages.
  if (!f || event.source !== f.el.contentWindow) return;
  const message = readFrameMessage(event.data);
  if (!message) return;
  if (message.type === "ready") {
    f.ready = true;
    stage.dataset.ready = "true";
    performance.mark("glass:artifact-ready");
    if (state.pinning) tell(f, { type: "pin", on: true });
    askWhere();
  } else if (message.type === "pick") {
    if (!state.pinning) return;
    state.pick = { path: message.path, x: message.x, y: message.y };
    setPinning(false);
    askWhere();
    renderComposer();
    composerText.focus();
  } else {
    // In the order asked; a path the page doesn't have comes back null.
    rects.clear();
    const wanted = new Set(asked);
    for (const r of message.rects) if (r && wanted.has(r.path)) rects.set(r.path, r);
    drawPins();
  }
});

function shownPins(): Pinned[] {
  const data = state.open?.data;
  return data && frame ? pinned(data.comments, frame.version) : [];
}

/** Asks the frame where the pins' elements are: their paths, nothing else. */
function askWhere(): void {
  const f = frame;
  if (!f?.ready) return;
  asked = wherePaths(shownPins(), state.pick?.path ?? null);
  if (asked.length === 0) {
    rects.clear();
    drawPins();
    return;
  }
  tell(f, { type: "where", paths: asked });
}

function drawPins(): void {
  const box = { w: stage.clientWidth, h: stage.clientHeight };
  const pins: HTMLElement[] = [];
  const found = new Set<string>();
  for (const p of shownPins()) {
    const r = rects.get(p.path);
    if (!r) continue;
    found.add(p.comment.id);
    const at = pinPosition(r, box);
    if (!at) continue;
    const pin = h(
      "button",
      {
        class: "pin",
        type: "button",
        "data-pin": p.n,
        "aria-label": `Comment ${p.n}, by ${p.comment.by_name}`,
        onclick: () => focusComment(p.comment.id),
      },
      String(p.n),
    );
    pin.style.left = `${at.left}px`;
    pin.style.top = `${at.top}px`;
    pins.push(pin);
  }
  const picking = state.pick?.path ? rects.get(state.pick.path) : undefined;
  const at = picking && pinPosition(picking, box);
  if (at) {
    const pin = h("span", { class: "pin pending", "data-test": "pending-pin" }, "+");
    pin.style.left = `${at.left}px`;
    pin.style.top = `${at.top}px`;
    pins.push(pin);
  }
  show(overlay, ...pins);
  // The list says which pinned comments the page no longer has.
  for (const badge of list.querySelectorAll<HTMLElement>("[data-badge]")) {
    const missing = Boolean(frame?.ready) && !found.has(badge.dataset.badge ?? "");
    badge.classList.toggle("missing", missing);
    badge.title = missing ? "Not found on this version of the page" : "Pinned on the page";
  }
}

function focusComment(id: string): void {
  const el = list.querySelector<HTMLElement>(`[data-comment="${CSS.escape(id)}"]`);
  if (!el) return;
  el.scrollIntoView({ block: "nearest", behavior: "smooth" });
  el.classList.remove("flash");
  void el.offsetWidth;
  el.classList.add("flash");
}

function setPinning(on: boolean): void {
  state.pinning = on;
  if (frame?.ready) tell(frame, { type: "pin", on });
  pickHint.hidden = !on;
  const toggle = head.querySelector<HTMLButtonElement>("[data-test=pin-toggle]");
  toggle?.setAttribute("aria-pressed", String(on));
  toggle?.classList.toggle("on", on);
}

// --- Loading -------------------------------------------------------

async function loadList(): Promise<void> {
  try {
    const result = await bridge.call<{ artifacts: Row[] }>("artifact.list", {});
    state.offline = false;
    if (!result.ok) {
      state.notice = bad(result.text);
      return;
    }
    state.rows = result.data.artifacts;
    if (state.notice?.tone === "bad" && !state.open) state.notice = null;
  } catch (error) {
    state.offline = error instanceof Offline;
    state.notice = { text: failure(error, "Artifacts"), tone: "bad" };
  }
}

/** Reads the open artifact: its versions and comments. The page itself is the frame's. */
async function loadArtifact(): Promise<void> {
  const open = state.open;
  if (!open) return;
  try {
    const result = await bridge.call<Artifact & { html?: string }>("artifact.read", { artifact: open.id });
    if (state.open !== open) return;
    if (!result.ok) {
      state.notice = bad(result.text);
      return;
    }
    const { html: _page, ...data } = result.data;
    const before = open.shown;
    open.data = data;
    if (!open.chosen || open.shown === null || !data.versions.some((v) => v.number === open.shown)) {
      open.shown = data.version;
      open.chosen = false;
    }
    if (before !== null && before !== open.shown) {
      state.notice = { text: `Now showing version ${open.shown}, shared ${when(data.versions.at(-1)?.at ?? null)}.`, tone: "ok" };
    }
    if (open.shown !== null) mountFrame(open.id, open.shown);
  } catch (error) {
    state.notice = { text: failure(error, "Opening it"), tone: "bad" };
  }
}

async function openArtifact(id: string): Promise<void> {
  if (state.open?.id !== id) {
    state.open = { id, data: null, shown: null, chosen: false };
    state.pick = null;
    state.replyTo = null;
    state.replyDraft = "";
    state.notice = null;
    composerText.value = "";
    setPinning(false);
  }
  if (location.hash !== `#${id}`) history.replaceState(null, "", `#${id}`);
  // The frame starts loading at once, beside the read of its comments.
  const row = state.rows.find((r) => r.id === id);
  if (state.open.shown === null && row?.version) {
    state.open.shown = row.version;
    mountFrame(id, row.version);
  }
  render();
  await loadArtifact();
  render();
  askWhere();
}

function showVersion(n: number): void {
  const open = state.open;
  if (!open?.data) return;
  open.shown = n;
  open.chosen = n !== open.data.version;
  state.pick = null;
  setPinning(false);
  mountFrame(open.id, n);
  render();
}

// --- Comments ------------------------------------------------------

async function post(input: Record<string, unknown>): Promise<boolean> {
  const open = state.open;
  if (!open) return false;
  state.busy = true;
  renderComposer();
  try {
    const result = await bridge.call<{ id: string }>("artifact.comment", { artifact: open.id, ...input });
    if (!result.ok) {
      state.notice = bad(result.text);
      return false;
    }
    if (state.notice?.tone === "bad") state.notice = null;
    await loadArtifact();
    return true;
  } catch (error) {
    state.notice = { text: `Not posted: ${failure(error, "Commenting")}`, tone: "bad" };
    return false;
  } finally {
    state.busy = false;
    render();
    askWhere();
  }
}

/** Posts what's in the composer, pinned where the person picked. */
async function comment(): Promise<void> {
  const text = composerText.value.trim();
  if (!text || state.busy) return;
  const pick = state.pick;
  const anchor = pick ? (pick.path ? { path: pick.path, x: pick.x, y: pick.y } : { x: pick.x, y: pick.y }) : null;
  if (await post(anchor ? { text, anchor } : { text })) {
    composerText.value = "";
    state.pick = null;
    render();
    askWhere();
  }
}

async function reply(to: string): Promise<void> {
  const text = state.replyDraft.trim();
  if (!text || state.busy) return;
  if (await post({ text, reply_to: to })) {
    state.replyTo = null;
    state.replyDraft = "";
    render();
  }
}

async function resolve(c: Comment): Promise<void> {
  const open = state.open;
  if (!open || state.busy) return;
  state.busy = true;
  try {
    const result = await bridge.call("artifact.resolve", { artifact: open.id, comment: c.id, resolved: !c.resolved });
    if (!result.ok) state.notice = bad(result.text);
    else await loadArtifact();
  } catch (error) {
    state.notice = { text: failure(error, "Resolving"), tone: "bad" };
  } finally {
    state.busy = false;
    render();
    askWhere();
  }
}

// --- Sharing -------------------------------------------------------

/** "q3-forecast.html" → "Q3 forecast". */
function titleFrom(name: string): string {
  const words = name.replace(/\.html?$/i, "").replace(/[-_]+/g, " ").trim();
  return words ? words[0]?.toUpperCase() + words.slice(1) : "";
}

/**
 * The share sheet: a new artifact (title, page, people or a conversation,
 * a note), or the owner's new version (page and note). The page is read
 * here, in the page, from a file chosen or dropped, or pasted.
 */
function openSheet(kind: "share" | "update"): void {
  const open = state.open;
  if (kind === "update" && !open?.data?.mine) return;
  root.querySelector(".sheet-backdrop")?.remove();
  setPinning(false);
  const page = { html: "", name: "" };
  const people = { picked: [] as Found[], found: [] as Found[], query: "" };
  let busy = false;

  const error = h("div", { class: "form-error", role: "alert", hidden: true });
  const say = (text: string | null) => {
    error.hidden = !text;
    show(error, text ?? "");
  };
  const title = h("input", { name: "title", autocomplete: "off", maxlength: 200, placeholder: "Q3 forecast", "aria-label": "Title" });
  const fileName = h("span", { class: "file-name", "data-test": "file-name" }, "No file chosen");
  const paste = h("textarea", {
    name: "html",
    rows: 5,
    spellcheck: "false",
    placeholder: "…or paste the page's HTML here",
    "aria-label": "The page's HTML",
    oninput: () => {
      if (page.name) {
        page.html = "";
        page.name = "";
        show(fileName, "No file chosen");
      }
    },
  });
  const readFile = (file: File | undefined) => {
    if (!file) return;
    if (!isHtmlFile(file)) return say(`${file.name} isn't an .html file. Artifacts are web pages, one file each.`);
    if (file.size > MAX_PAGE_BYTES) return say(`${file.name} is ${size(file.size)}; a page is ${size(MAX_PAGE_BYTES)} at most.`);
    const reader = new FileReader();
    reader.onload = () => {
      page.html = String(reader.result ?? "");
      page.name = file.name;
      paste.value = "";
      show(fileName, `${file.name} · ${size(file.size)}`);
      if (kind === "share" && !title.value.trim()) title.value = titleFrom(file.name);
      say(null);
    };
    reader.onerror = () => say(`${file.name} couldn't be read.`);
    reader.readAsText(file);
  };
  const fileInput = h("input", {
    type: "file",
    name: "file",
    accept: ".html,.htm,text/html",
    class: "visually-hidden",
    onchange: (e: Event) => readFile((e.target as HTMLInputElement).files?.[0]),
  });
  const drop = h(
    "label",
    {
      class: "drop",
      "data-test": "drop",
      ondragover: (e: Event) => {
        e.preventDefault();
        drop.classList.add("over");
      },
      ondragleave: () => drop.classList.remove("over"),
      ondrop: (e: Event) => {
        e.preventDefault();
        drop.classList.remove("over");
        readFile((e as DragEvent).dataTransfer?.files[0]);
      },
    },
    fileInput,
    h("span", { class: "page-icon", "aria-hidden": "true" }),
    h("span", { class: "drop-text" }, "Drop an .html file here, or ", h("span", { class: "link" }, "choose one")),
    fileName,
  );

  // People, as they're typed (the org's directory), or a conversation.
  const chips = h("span", { class: "chips" });
  const suggestions = h("div", { class: "suggestions", role: "listbox", hidden: true });
  const who = h("input", {
    name: "people",
    autocomplete: "off",
    placeholder: "Type a name",
    "aria-label": "People",
    oninput: (e: Event) => void search((e.target as HTMLInputElement).value),
  });
  const drawPeople = () => {
    show(
      chips,
      ...people.picked.map((p) =>
        h(
          "span",
          { class: "chip", "data-person": p.login },
          p.name,
          h(
            "button",
            {
              type: "button",
              class: "chip-x",
              "aria-label": `Remove ${p.name}`,
              onclick: () => {
                people.picked = people.picked.filter((x) => x.login !== p.login);
                drawPeople();
              },
            },
            "×",
          ),
        ),
      ),
    );
    suggestions.hidden = people.found.length === 0;
    show(
      suggestions,
      ...people.found.map((p) =>
        h(
          "button",
          {
            type: "button",
            class: "suggestion",
            role: "option",
            "data-login": p.login,
            onclick: () => {
              people.picked.push(p);
              people.found = [];
              people.query = "";
              who.value = "";
              drawPeople();
              who.focus();
            },
          },
          h("span", { class: "avatar small" }, initials(p.name)),
          h("span", { class: "who" }, p.name),
          p.title ? h("span", { class: "muted" }, ` · ${p.title}`) : null,
        ),
      ),
    );
  };
  async function search(query: string): Promise<void> {
    people.query = query;
    if (!query.trim()) {
      people.found = [];
      return drawPeople();
    }
    try {
      const result = await bridge.call<{ people: Found[] }>("people.search", { query, limit: 8 });
      if (people.query !== query) return;
      people.found = result.ok ? result.data.people.filter((f) => f.login !== state.me && !people.picked.some((x) => x.login === f.login)) : [];
    } catch {
      people.found = [];
    }
    drawPeople();
  }
  const label = h(
    "select",
    { name: "label", "aria-label": "Label" },
    ...SHARE_LABELS.map((l) => {
      const option = h("option", { value: l }, labelName(l) ?? l);
      option.selected = l === "internal";
      return option;
    }),
  );
  const conversation = h("select", { name: "conversation", "aria-label": "Conversation" }, h("option", { value: "" }, "No conversation"));
  const conversationNote = h("span", { class: "muted small" });
  if (kind === "share") {
    void bridge
      .call<{ conversations: Conversation[] }>("chat.list", {})
      .then((result) => {
        const found = result.ok ? result.data.conversations : [];
        for (const c of found) conversation.append(h("option", { value: c.id }, c.title));
        conversation.disabled = found.length === 0;
        show(conversationNote, found.length ? "Everyone in it can see it, and it shows there as a card." : "No conversations in Chat yet.");
      })
      .catch(() => show(conversationNote, "Chat can't be reached right now."));
  }
  const note = h(kind === "share" ? "input" : "textarea", {
    name: "note",
    autocomplete: "off",
    maxlength: MAX_TEXT_CHARS,
    placeholder: kind === "share" ? "What it is, or what you'd like feedback on" : "What changed",
    "aria-label": "Note",
  });
  const next = (open?.data?.version ?? 0) + 1;
  const submit = h("button", { class: "button primary", type: "submit", "data-test": "sheet-submit" }, kind === "share" ? "Share" : `Share version ${next}`);
  const asks = bridge.asks(kind === "share" ? "artifact.share" : "artifact.update");
  const close = () => {
    backdrop.remove();
    document.removeEventListener("keydown", escape);
  };
  const escape = (e: KeyboardEvent) => {
    if (e.key === "Escape") close();
  };
  document.addEventListener("keydown", escape);

  async function send(): Promise<void> {
    if (busy) return;
    const html = page.html || paste.value;
    const checked =
      kind === "share"
        ? shareInput({
            title: title.value,
            label: label.value as Label,
            html,
            with: people.picked.map((p) => p.login),
            conversation: conversation.value || null,
            note: note.value,
          })
        : updateInput(open?.id ?? "", html, note.value);
    if ("problem" in checked) return say(checked.problem);
    busy = true;
    submit.disabled = true;
    say(null);
    show(submit, asks ? "Waiting for your approval on the desktop…" : "Sharing…");
    try {
      const result =
        kind === "share"
          ? await bridge.call<{ id: string }>("artifact.share", checked.input)
          : await bridge.call<{ id: string; version: number }>("artifact.update", checked.input);
      if (!result.ok) {
        say(readable(result.text));
        return;
      }
      close();
      state.notice = { text: readable(result.text), tone: "ok" };
      await loadList();
      if (kind === "share") {
        await openArtifact(result.data.id);
        state.notice = { text: readable(result.text), tone: "ok" };
        render();
      } else {
        if (state.open) state.open.chosen = false;
        await loadArtifact();
        render();
        askWhere();
      }
    } catch (error) {
      say(failure(error, "Sharing"));
    } finally {
      busy = false;
      submit.disabled = false;
      show(submit, kind === "share" ? "Share" : `Share version ${next}`);
    }
  }

  const field = (label: string, ...children: (Node | string | null)[]) => h("div", { class: "field" }, h("span", { class: "field-label" }, label), ...children);
  const sheet = h(
    "form",
    {
      class: "sheet",
      role: "dialog",
      "aria-modal": "true",
      "aria-label": kind === "share" ? "Share a page" : "New version",
      "data-test": "sheet",
      onsubmit: (e: Event) => {
        e.preventDefault();
        void send();
      },
    },
    h("h2", {}, kind === "share" ? "Share a page" : `New version of ${open?.data?.title ?? "this artifact"}`),
    h(
      "p",
      { class: "muted sheet-about" },
      kind === "share"
        ? "It opens for them here in a sealed frame: its own scripts run, but it can't reach the network or anyone's data, so inline what it needs."
        : "Everyone it's shared with sees the new version. Comments on earlier ones stay, marked with their version.",
    ),
    kind === "share" ? field("Title", title) : null,
    field("Page", drop, paste),
    kind === "share" ? field("With", h("div", { class: "people-field" }, chips, who), suggestions) : null,
    kind === "share" ? field("Or in", conversation, conversationNote) : null,
    kind === "share"
      ? field("Label", label, h("span", { class: "muted small" }, "Who may see it, and what may read it."))
      : null,
    field(kind === "share" ? "Note" : "What changed", note),
    error,
    h(
      "div",
      { class: "sheet-actions" },
      asks ? h("span", { class: "muted small" }, "Your organization asks you to approve this on the desktop.") : h("span", {}),
      h("button", { class: "button", type: "button", onclick: close }, "Cancel"),
      submit,
    ),
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
  root.append(backdrop);
  (kind === "share" ? title : note).focus();
}

// --- Drawing -------------------------------------------------------

/** The data label's badge (DP): Public, Internal, Confidential. */
function labelTag(label: unknown): HTMLElement | null {
  const name = labelName(label);
  return name ? h("span", { class: `label-tag ${String(label)}`, "data-test": "label", title: `Labeled ${name}` }, name) : null;
}

function row(r: Row): HTMLElement {
  const active = state.open?.id === r.id;
  return h(
    "button",
    { class: `row${active ? " active" : ""}`, "data-artifact": r.id, onclick: () => void openArtifact(r.id) },
    h("span", { class: "page-icon small", "aria-hidden": "true" }),
    h(
      "span",
      { class: "row-main" },
      h("span", { class: "row-title" }, r.title),
      h(
        "span",
        { class: "row-meta" },
        labelTag(r.label),
        [r.mine ? "You" : r.owner_name, r.version ? `v${r.version}` : null, when(r.updated_at)].filter(Boolean).join(" · "),
      ),
    ),
    r.open_comments
      ? h("span", { class: "badge", title: `${r.open_comments} open comment${r.open_comments === 1 ? "" : "s"}`, "data-test": "open-comments" }, String(r.open_comments))
      : null,
  );
}

function renderSidebar(): void {
  const { shared, mine } = partition(state.rows);
  show(
    sidebar,
    h(
      "div",
      { class: "side-head" },
      h("h1", {}, "Artifacts"),
      h("button", { class: "button small", "data-test": "share", onclick: () => openSheet("share") }, "Share"),
    ),
    h(
      "section",
      { class: "side-section", "data-test": "shared-with-me" },
      h("h2", {}, "Shared with me"),
      ...(shared.length ? shared.map(row) : [h("div", { class: "side-empty" }, "Nothing shared with you yet")]),
    ),
    h(
      "section",
      { class: "side-section", "data-test": "mine" },
      h("h2", {}, "Mine"),
      ...(mine.length ? mine.map(row) : [h("div", { class: "side-empty" }, "Pages you share show here")]),
    ),
    h("footer", { class: "about" }, "Each page opens sealed in a frame of its own: its scripts run, but it can't reach the network or your data."),
  );
}

function renderWelcome(): void {
  show(
    welcome,
    h(
      "div",
      { class: "empty welcome", "data-test": "welcome" },
      h("span", { class: "page-icon large", "aria-hidden": "true" }),
      h("h2", {}, state.rows.length ? "Pick an artifact" : "No artifacts yet"),
      h(
        "p",
        { class: "muted" },
        "Artifacts are pages people and their agents make to be looked at: reports, charts, small tools. Instead of files sent in chat, they're shared here, open in a sealed frame that can't reach the network or your data, and keep everyone's comments with them.",
      ),
      h("p", { class: "muted" }, "When your agent makes a page for you, or a colleague shares one, it shows up here."),
      h("button", { class: "button primary", onclick: () => openSheet("share") }, "Share a page"),
    ),
  );
}

function renderHead(): void {
  const open = state.open;
  if (!open) {
    show(head, h("h1", {}, "Artifacts"));
    return;
  }
  const data = open.data;
  const r = state.rows.find((x) => x.id === open.id);
  const title = data?.title ?? r?.title ?? "Artifact";
  const owner = data ? (data.mine ? "You" : data.owner_name) : r ? (r.mine ? "You" : r.owner_name) : "";
  const others = (data?.people ?? r?.people ?? []).filter((p) => p.login !== state.me && p.login !== (data?.owner ?? r?.owner));
  const latest = data?.version ?? null;
  const older = latest !== null && open.shown !== null && open.shown !== latest;
  const versions = h(
    "select",
    {
      class: "versions",
      "aria-label": "Version",
      "data-test": "versions",
      onchange: (e: Event) => showVersion(Number((e.target as HTMLSelectElement).value)),
    },
    ...[...(data?.versions ?? [])].reverse().map((v) => {
      const option = h("option", { value: v.number }, `Version ${v.number}${v.number === latest ? " (latest)" : ""}`);
      option.selected = v.number === open.shown;
      return option;
    }),
  );
  versions.disabled = !data || data.versions.length < 2;
  const pin = h(
    "button",
    {
      class: `button toggle${state.pinning ? " on" : ""}`,
      type: "button",
      "aria-pressed": String(state.pinning),
      "data-test": "pin-toggle",
      title: older ? "Comments go on the latest version; switch to it to pin one" : "Pin a comment to something on the page",
      onclick: () => setPinning(!state.pinning),
    },
    h("span", { class: "pin-icon", "aria-hidden": "true" }),
    "Pin a comment",
  );
  pin.disabled = older || !frame;
  show(
    head,
    h(
      "div",
      { class: "head-title" },
      h("h1", { "data-test": "title" }, title),
      labelTag(data?.label ?? r?.label),
      h(
        "span",
        { class: "muted head-meta" },
        owner ? `${owner === "You" ? "Yours" : `By ${owner}`}${others.length ? ` · with ${others.map((p) => p.name).join(", ")}` : ""}` : "",
      ),
    ),
    h("div", { class: "head-actions" }, versions, pin, data?.mine ? h("button", { class: "button", type: "button", "data-test": "new-version", onclick: () => openSheet("update") }, "New version") : null),
  );
}

/** The shown version's note and who shared it. */
function renderVersionLine(): void {
  const data = state.open?.data;
  const v = data?.versions.find((x) => x.number === state.open?.shown);
  if (!data || !v) {
    show(versionLine);
    versionLine.hidden = true;
    return;
  }
  const by = parseAttribution(v.attribution);
  versionLine.hidden = false;
  show(
    versionLine,
    h("span", { class: "tag" }, `v${v.number}`),
    v.note ? h("span", { class: "version-note" }, v.note) : null,
    h("span", { class: "muted" }, `${v.by === state.me ? "You" : v.by_name}, ${when(v.at)}`),
    by && showAttribution(by, "artifacts") ? attributionChip(by.text) : null,
    v.number !== data.version ? h("span", { class: "tag older" }, `An earlier version: v${data.version} is the latest`) : null,
  );
}

function commentView(c: Comment, shown: number, pin: Pinned | undefined, reply: boolean): HTMLElement {
  const by = parseAttribution(c.attribution);
  const badge = versionBadge(c, shown);
  const open = state.open;
  return h(
    "div",
    { class: `comment${reply ? " reply" : ""}`, "data-comment": c.id },
    h(
      "div",
      { class: "comment-head" },
      pin ? h("span", { class: "pin-badge", "data-badge": c.id }, String(pin.n)) : null,
      h("span", { class: "avatar small" }, initials(c.by_name)),
      h("span", { class: "who" }, c.by === state.me ? `${c.by_name} (you)` : c.by_name),
      h("span", { class: "time" }, when(c.at)),
      badge ? h("span", { class: "tag", "data-test": "version-badge" }, badge) : null,
    ),
    by && showAttribution(by, "artifacts") ? h("div", { class: "comment-by" }, attributionChip(by.text)) : null,
    h("p", { class: "comment-text" }, c.text),
    reply || !open
      ? null
      : h(
          "div",
          { class: "comment-actions" },
          h(
            "button",
            {
              class: "link",
              type: "button",
              onclick: () => {
                state.replyTo = state.replyTo === c.id ? null : c.id;
                state.replyDraft = "";
                renderComments();
                list.querySelector<HTMLTextAreaElement>("[name=reply]")?.focus();
              },
            },
            "Reply",
          ),
          h("button", { class: "link", type: "button", "data-test": c.resolved ? "reopen" : "resolve", onclick: () => void resolve(c) }, c.resolved ? "Reopen" : "Resolve"),
        ),
  );
}

function replyBox(to: string): HTMLElement {
  const text = h("textarea", {
    name: "reply",
    rows: 2,
    maxlength: MAX_TEXT_CHARS,
    placeholder: "Reply",
    "aria-label": "Reply",
    oninput: (e: Event) => {
      state.replyDraft = (e.target as HTMLTextAreaElement).value;
    },
    onkeydown: (e: Event) => {
      const key = e as KeyboardEvent;
      if (key.key === "Enter" && (key.metaKey || key.ctrlKey)) {
        e.preventDefault();
        void reply(to);
      }
    },
  });
  text.value = state.replyDraft;
  return h(
    "form",
    {
      class: "reply-box",
      onsubmit: (e: Event) => {
        e.preventDefault();
        void reply(to);
      },
    },
    text,
    h(
      "div",
      { class: "composer-row" },
      h(
        "button",
        {
          class: "button small",
          type: "button",
          onclick: () => {
            state.replyTo = null;
            renderComments();
          },
        },
        "Cancel",
      ),
      h("button", { class: "button primary small", type: "submit" }, "Reply"),
    ),
  );
}

function renderComments(): void {
  const data = state.open?.data;
  const shown = state.open?.shown ?? data?.version ?? 0;
  const focused = document.activeElement instanceof HTMLTextAreaElement && list.contains(document.activeElement);
  if (!data) {
    show(commentsHead, h("h2", {}, "Comments"));
    show(list, h("div", { class: "comments-empty" }, state.open && !state.notice ? "Opening…" : ""));
    return;
  }
  const pins = new Map(pinned(data.comments, shown).map((p) => [p.comment.id, p]));
  const all = threads(data.comments);
  // Open ones first; resolved ones after them, quieter.
  const ordered = [...all.filter((t) => !t.comment.resolved), ...all.filter((t) => t.comment.resolved)];
  const open = data.comments.filter((c) => !c.reply_to && !c.resolved).length;
  show(commentsHead, h("h2", {}, "Comments"), h("span", { class: "muted" }, open ? `${open} open` : data.comments.length ? "All resolved" : ""));
  show(
    list,
    ...(ordered.length
      ? ordered.map((t) =>
          h(
            "article",
            { class: `thread${t.comment.resolved ? " resolved" : ""}`, "data-thread": t.comment.id },
            commentView(t.comment, shown, pins.get(t.comment.id), false),
            ...t.replies.map((r) => commentView(r, shown, undefined, true)),
            state.replyTo === t.comment.id ? replyBox(t.comment.id) : null,
          ),
        )
      : [h("div", { class: "comments-empty" }, "No comments yet. Pin one to something on the page, or write one below.")]),
  );
  if (focused) {
    const box = list.querySelector<HTMLTextAreaElement>("[name=reply]");
    box?.focus();
    box?.setSelectionRange(box.value.length, box.value.length);
  }
}

function renderComposer(): void {
  const open = state.open;
  const data = open?.data;
  const pick = state.pick;
  anchorChip.hidden = !pick;
  show(
    anchorChip,
    h("span", { class: "pin-icon", "aria-hidden": "true" }),
    pick?.path ? "Pinned to what you clicked" : "Pinned to a spot on the page",
    h(
      "button",
      {
        class: "chip-x",
        type: "button",
        "aria-label": "Don't pin it",
        onclick: () => {
          state.pick = null;
          renderComposer();
          askWhere();
        },
      },
      "×",
    ),
  );
  const older = data && open?.shown !== null && open?.shown !== data.version;
  show(
    composerHint,
    older ? `Comments go on version ${data?.version}, the latest.` : bridge.asks("artifact.comment") ? "Each comment waits for your approval on the desktop." : "Ctrl+Enter to post",
  );
  composerText.disabled = !data;
  composerButton.disabled = !data || state.busy;
}

function render(): void {
  renderSidebar();
  renderHead();
  show(
    banners,
    state.notice && h("div", { class: `banner ${state.notice.tone === "ok" ? "done" : "refused"}`, role: "status", "data-test": "notice" }, state.notice.text),
    state.offline && h("div", { class: "banner offline" }, "Can't reach the system right now. Nothing you write is sent until it's back."),
  );
  const open = Boolean(state.open);
  welcome.hidden = open;
  viewer.hidden = !open;
  if (!open) {
    renderWelcome();
    if (frame) unmountFrame();
  }
  renderVersionLine();
  renderComments();
  renderComposer();
  drawPins();
}

/** Waits for news and reads it: the list, and the open artifact, per change. */
function live(): void {
  watch(
    bridge,
    "artifacts",
    state.cursor,
    async () => {
      // A change while a comment is on its way is read once that ends,
      // never dropped (NV-3, ADR 0051).
      while (state.busy) await new Promise((resolve) => setTimeout(resolve, 200));
      await loadList();
      await loadArtifact();
      render();
      askWhere();
    },
    {
      onOffline: (offline) => {
        if (!offline) return;
        state.offline = true;
        render();
      },
      onAnswer: () => {
        if (!state.offline) return;
        state.offline = false;
        render();
      },
    },
  );
}

async function start(): Promise<void> {
  show(root, sidebar, h("main", { class: "pane" }, head, banners, body));
  window.addEventListener("hashchange", () => {
    const id = deepLink(location.hash);
    if (id && id !== state.open?.id) void openArtifact(id);
  });
  window.addEventListener("resize", () => drawPins());
  document.addEventListener("keydown", (e: KeyboardEvent) => {
    if (e.key === "Escape" && state.pinning) setPinning(false);
  });
  try {
    const session = await bridge.open();
    state.me = session.user;
  } catch (error) {
    state.notice = { text: failure(error, "Artifacts"), tone: "bad" };
  }
  // The cursor before the first load, so a change in between is news
  // (NV-3, ADR 0051).
  // Offline, 0: live() says so.
  state.cursor = await firstCursor(bridge);
  await loadList();
  const linked = deepLink(location.hash);
  if (linked) await openArtifact(linked);
  else render();
  ready();
  live();
}

void start();
