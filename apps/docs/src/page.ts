// Docs (DC-1, DC-3, DC-4, ADR 0035): pages people write together, live.
// The list of docs shared with the person and their own; the open doc in
// a clean block editor; its comments and named versions beside it; and
// sharing when a doc is made.
//
// A doc is opened with `doc.open` (an audited action): its whole state as
// one Yjs update, a cursor and the person's role. From then on the page
// pushes and pulls Yjs updates over `/_bridge/live` (live.ts), which is
// not an action, so typing doesn't write an audit record per keystroke.
// Comments, versions and roles are actions. Like Chat and Artifacts it
// keeps nothing: news of other changes comes from `app.status` waits.
//
// `mount` takes the bridge, so a page with a stand-in bridge can show the
// app without a system.

import { attributionChip, type Bridge, BridgeError, h, type LivePulled, type LivePushed, Offline, parseAttribution, ready, show, showAttribution } from "@glasshouse/sdk";
import * as Y from "yjs";

import {
  type Comment,
  type Found,
  type Invitee,
  type Label,
  type Opened,
  type Person,
  type Role,
  type Row,
  type Share,
  artifactFrom,
  canComment,
  canEdit,
  commentInput,
  createInput,
  DOC_LABELS,
  deepLink,
  initials,
  labelName,
  MAX_TEXT_CHARS,
  MAX_TITLE_CHARS,
  MAX_VERSION_CHARS,
  names,
  partition,
  readable,
  roleName,
  rolesInput,
  threads,
  versionInput,
  when,
} from "./docs.ts";
import { Editor, type Hooks } from "./editor.ts";
import { applyState, type Health, LiveSync } from "./live.ts";

type Meta = Omit<Opened, "state" | "cursor">;

interface Open {
  id: string;
  meta: Meta | null;
  ydoc: Y.Doc | null;
  editor: Editor | null;
  live: LiveSync | null;
  health: Health;
  why: string | null;
  /** When someone else last changed it, while open. */
  touched: { by_name: string; at: number } | null;
}

/** This page, to glassd's live session: its own pushes don't come back to it, others' windows' do. */
const CLIENT = [...crypto.getRandomValues(new Uint8Array(16))].map((b) => b.toString(16).padStart(2, "0")).join("");

/** How soon the list and the open doc's comments and versions are read again, on news. */
const REFRESH_MS = 1500;
/**
 * While the doc is being typed in, news is mostly those edits, which the
 * live pull already brought: the reads (each an audited action) wait this
 * long instead. Comments and versions from others still arrive.
 */
const BUSY_REFRESH_MS = 30_000;
/** How recent an edit makes the doc "being typed in". */
const TYPING_MS = 4000;

let bridge: Bridge;
let root: HTMLElement;
/** The next read on news, and when the last one was. */
let refreshing: ReturnType<typeof setTimeout> | null = null;
let refreshedAt = 0;

const state = {
  me: "",
  rows: [] as Row[],
  loaded: false,
  open: null as Open | null,
  panel: null as "comments" | "versions" | null,
  /** A comment being written on a selection: where, and what it quotes. */
  draft: null as { path: string; quote: string } | null,
  /** The thread shown in the panel and highlighted in the doc. */
  active: null as string | null,
  replyTo: null as string | null,
  replyDraft: "",
  showResolved: false,
  busy: false,
  notice: null as { text: string; tone: "bad" | "ok" } | null,
  offline: false,
  cursor: 0,
};

// The parts that redraw. The editor and the composers stay, so news never
// interrupts typing.
const sidebar = h("aside", { class: "sidebar" });
const head = h("header", { class: "pane-head" });
const banners = h("div", { class: "banners" });
const welcome = h("div", { class: "welcome-wrap" });
const docTop = h("div", { class: "doc-top" });
const editorMount = h("div", { class: "editor-mount" });
const page = h("article", { class: "doc-page", "data-test": "doc" }, docTop, editorMount);
const scroller = h("div", { class: "doc-scroll" }, page);
const panelHead = h("div", { class: "panel-head" });
const panelBody = h("div", { class: "panel-body" });
const panelFoot = h("div", { class: "panel-foot" });
const panel = h("aside", { class: "panel", "data-test": "panel", hidden: true }, panelHead, panelBody, panelFoot);
const docView = h("div", { class: "doc-view", hidden: true }, scroller, panel);
const body = h("div", { class: "pane-body" }, welcome, docView);

const draftText = h("textarea", {
  class: "composer-text",
  name: "draft",
  rows: 3,
  maxlength: MAX_TEXT_CHARS,
  placeholder: "Add a comment",
  "aria-label": "Comment on the selection",
  onkeydown: (e: Event) => {
    const key = e as KeyboardEvent;
    if (key.key === "Enter" && (key.metaKey || key.ctrlKey)) {
      e.preventDefault();
      void postDraft();
    } else if (key.key === "Escape") {
      state.draft = null;
      renderPanel();
      state.open?.editor?.focus();
    }
  },
});
const generalText = h("textarea", {
  class: "composer-text",
  name: "comment",
  rows: 2,
  maxlength: MAX_TEXT_CHARS,
  placeholder: "Comment on the whole doc",
  "aria-label": "Comment",
  onkeydown: (e: Event) => {
    const key = e as KeyboardEvent;
    if (key.key === "Enter" && (key.metaKey || key.ctrlKey)) {
      e.preventDefault();
      void postGeneral();
    }
  },
});
const versionName = h("input", { name: "version", autocomplete: "off", maxlength: MAX_VERSION_CHARS, placeholder: "Sent to finance", "aria-label": "Version name" });

function failure(error: unknown, doing: string): string {
  if (error instanceof Offline) return `${doing} needs the system, and it isn't answering.`;
  if (error instanceof BridgeError) return readable(error.message);
  return readable(String(error));
}

function bad(text: string): { text: string; tone: "bad" } {
  return { text: readable(text), tone: "bad" };
}

// --- Loading ----------------------------------------------------------------

async function loadList(): Promise<void> {
  try {
    const result = await bridge.call<{ docs: Row[] }>("doc.list", {});
    state.offline = false;
    state.loaded = true;
    if (!result.ok) {
      state.notice = bad(result.text);
      return;
    }
    state.rows = result.data.docs;
    if (state.notice?.tone === "bad" && !state.open) state.notice = null;
  } catch (error) {
    state.offline = error instanceof Offline;
    state.notice = { text: failure(error, "Docs"), tone: "bad" };
  }
}

/** Takes in what doc.open said about the open doc (not its state: the caller merges that). */
function took(open: Open, data: Opened): void {
  const { state: _state, cursor: _cursor, ...meta } = data;
  const role = open.meta?.role;
  open.meta = meta;
  const row = state.rows.find((r) => r.id === open.id);
  if (row) {
    row.title = meta.title;
    row.role = meta.role;
  }
  if (role && role !== meta.role && open.ydoc) {
    // A new role: a new live session, in that role (ADR 0035 §3).
    open.live?.stop();
    open.live = startLive(open, data.cursor);
  }
  if (state.active && !meta.comments.some((c) => c.id === state.active)) state.active = null;
  open.editor?.setComments(meta.comments, state.active);
  open.editor?.refresh();
}

function startLive(open: Open, cursor: number): LiveSync {
  return new LiveSync({
    doc: open.ydoc as Y.Doc,
    cursor,
    readOnly: !canEdit(open.meta?.role),
    channel: {
      push: (update) => bridge.live<LivePushed>({ op: "push", doc: open.id, update, client: CLIENT }),
      pull: (since, waitSecs) => bridge.live<LivePulled>({ op: "pull", doc: open.id, since, wait_secs: waitSecs, client: CLIENT }),
      reopen: async () => {
        const result = await bridge.call<Opened>("doc.open", { doc: open.id });
        if (!result.ok) {
          state.notice = bad(result.text);
          render();
          return null;
        }
        took(open, result.data);
        return { state: result.data.state, cursor: result.data.cursor };
      },
    },
    onHealth: (health, why) => {
      open.health = health;
      open.why = why ? readable(why) : null;
      open.editor?.refresh();
      if (state.open === open) renderHead();
    },
    onRemote: (from) => {
      const other = from.filter((f) => f.by !== state.me).at(-1);
      if (other) open.touched = { by_name: other.by_name, at: Date.now() };
      if (state.open === open) renderDocTop();
    },
    onReopen: () => {
      if (state.open === open) render();
    },
  }).start();
}

function hooks(open: Open): Hooks {
  return {
    editable: () => state.open === open && canEdit(open.meta?.role) && open.health !== "offline" && open.health !== "refused",
    canComment: () => canComment(open.meta?.role),
    comment: (at) => {
      state.draft = at;
      state.panel = "comments";
      state.active = null;
      render();
      draftText.focus();
    },
    focusComment: (id) => focusThread(id, false),
    openDoc: (id) => void openDoc(id),
    titleOf: (id) => state.rows.find((r) => r.id === id)?.title ?? null,
    pickDoc: () => pickDoc(),
    pickArtifact: () => pickArtifact(),
  };
}

function closeDoc(): void {
  const open = state.open;
  if (!open) return;
  open.editor?.destroy();
  open.editor = null;
  state.open = null;
  // Whatever is still unsent goes before the session stops, and the doc
  // is kept until it has: with the system away that takes as long as it
  // is away. (It was sent once and the doc destroyed, so closing a doc,
  // or opening another, while offline threw away what hadn't gone.)
  const { live, ydoc } = open;
  if (live) void live.finish().then(() => ydoc?.destroy());
  else ydoc?.destroy();
}

async function openDoc(id: string): Promise<void> {
  if (state.open?.id === id && state.open.meta) return;
  closeDoc();
  const open: Open = { id, meta: null, ydoc: null, editor: null, live: null, health: "live", why: null, touched: null };
  state.open = open;
  state.draft = null;
  state.active = null;
  state.replyTo = null;
  state.replyDraft = "";
  state.notice = null;
  draftText.value = "";
  generalText.value = "";
  if (location.hash !== `#${id}`) history.replaceState(null, "", `#${id}`);
  page.dataset.ready = "false";
  render();
  let result;
  try {
    result = await bridge.call<Opened>("doc.open", { doc: id });
  } catch (error) {
    if (state.open !== open) return;
    state.notice = { text: failure(error, "Opening it"), tone: "bad" };
    render();
    return;
  }
  if (state.open !== open) return;
  if (!result.ok) {
    state.notice = bad(result.text);
    render();
    return;
  }
  const data = result.data;
  const ydoc = new Y.Doc();
  try {
    applyState(ydoc, data.state);
  } catch {
    state.notice = bad("This doc's contents couldn't be read.");
    render();
    return;
  }
  open.ydoc = ydoc;
  refreshedAt = Date.now();
  took(open, data);
  open.editor = new Editor(editorMount, ydoc, hooks(open));
  open.live = startLive(open, data.cursor);
  open.editor.setComments(data.comments, null);
  render();
  page.dataset.ready = "true";
  page.dataset.open = id;
  performance.mark("glass:doc-open");
  if (canEdit(data.role) && isEmpty(ydoc)) open.editor.focus();
}

function isEmpty(ydoc: Y.Doc): boolean {
  return ydoc.getXmlFragment("prosemirror").length === 0;
}

/** Reads the open doc's comments, versions and people again (doc.open; its state merges harmlessly). */
async function refreshDoc(): Promise<void> {
  const open = state.open;
  if (!open?.ydoc) return;
  refreshedAt = Date.now();
  try {
    const result = await bridge.call<Opened>("doc.open", { doc: open.id });
    if (state.open !== open || !open.ydoc) return;
    if (!result.ok) {
      state.notice = bad(result.text);
      return;
    }
    applyState(open.ydoc, result.data.state);
    took(open, result.data);
  } catch (error) {
    state.notice = { text: failure(error, "Reading it again"), tone: "bad" };
  }
}

/**
 * News arrived (app.status says Docs changed): the list and the open
 * doc's comments and versions are read again, shortly. When the open doc
 * was being typed in, the news was most likely those edits, so the reads
 * wait longer (BUSY_REFRESH_MS) rather than writing an audit record every
 * few seconds while people type.
 */
function soon(delay = REFRESH_MS): void {
  if (refreshing) return;
  refreshing = setTimeout(() => {
    refreshing = null;
    const live = state.open?.live;
    const typing = live && Date.now() - live.activeAt < TYPING_MS;
    const wait = typing ? refreshedAt + BUSY_REFRESH_MS - Date.now() : 0;
    if (wait > 0) return soon(wait);
    refreshedAt = Date.now();
    void (async () => {
      await loadList();
      if (!state.busy) await refreshDoc();
      render();
    })();
  }, delay);
}

// --- Comments ------------------------------------------------------------------

async function post(input: Record<string, unknown>): Promise<boolean> {
  state.busy = true;
  renderPanel();
  try {
    const result = await bridge.call<{ id: string }>("doc.comment", input);
    if (!result.ok) {
      state.notice = bad(result.text);
      return false;
    }
    if (state.notice?.tone === "bad") state.notice = null;
    await refreshDoc();
    if (result.data?.id && !input.reply_to) state.active = result.data.id;
    return true;
  } catch (error) {
    state.notice = { text: `Not posted: ${failure(error, "Commenting")}`, tone: "bad" };
    return false;
  } finally {
    state.busy = false;
    state.open?.editor?.setComments(state.open.meta?.comments ?? [], state.active);
    render();
  }
}

async function postDraft(): Promise<void> {
  const open = state.open;
  const draft = state.draft;
  if (!open || !draft || state.busy) return;
  const checked = commentInput(open.id, draftText.value, { path: draft.path });
  if ("problem" in checked) return;
  if (await post(checked.input)) {
    draftText.value = "";
    state.draft = null;
    render();
  }
}

async function postGeneral(): Promise<void> {
  const open = state.open;
  if (!open || state.busy) return;
  const checked = commentInput(open.id, generalText.value);
  if ("problem" in checked) return;
  if (await post(checked.input)) {
    generalText.value = "";
    render();
  }
}

async function reply(to: string): Promise<void> {
  const open = state.open;
  if (!open || state.busy) return;
  const checked = commentInput(open.id, state.replyDraft, { replyTo: to });
  if ("problem" in checked) return;
  if (await post(checked.input)) {
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
    const result = await bridge.call("doc.resolve", { doc: open.id, comment: c.id, resolved: !c.resolved });
    if (!result.ok) state.notice = bad(result.text);
    else {
      if (!c.resolved && state.active === c.id) state.active = null;
      await refreshDoc();
    }
  } catch (error) {
    state.notice = { text: failure(error, "Resolving"), tone: "bad" };
  } finally {
    state.busy = false;
    render();
  }
}

/** Shows a thread in the panel and its place in the doc. */
function focusThread(id: string, scrollDoc: boolean): void {
  const open = state.open;
  state.panel = "comments";
  state.active = id;
  open?.editor?.setComments(open.meta?.comments ?? [], id);
  render();
  const el = panelBody.querySelector<HTMLElement>(`[data-thread="${CSS.escape(id)}"]`);
  el?.scrollIntoView({ block: "nearest", behavior: "smooth" });
  el?.classList.remove("flash");
  void el?.offsetWidth;
  el?.classList.add("flash");
  if (!scrollDoc) return;
  const c = open?.meta?.comments.find((x) => x.id === id);
  const pos = open?.editor?.positionOf(c?.anchor?.path);
  const at = pos !== null && pos !== undefined ? open?.editor?.coordsOf(pos) : null;
  if (!at) return;
  const box = scroller.getBoundingClientRect();
  if (at.top < box.top + 40 || at.bottom > box.bottom - 40) scroller.scrollBy({ top: at.top - box.top - box.height / 3, behavior: "smooth" });
}

// --- Versions --------------------------------------------------------------------

async function nameVersion(): Promise<void> {
  const open = state.open;
  if (!open || state.busy) return;
  const checked = versionInput(open.id, versionName.value);
  if ("problem" in checked) {
    state.notice = bad(checked.problem);
    renderBanners();
    return;
  }
  state.busy = true;
  renderPanel();
  try {
    // What this page has typed reaches the system first, so the version holds it.
    await open.live?.flush();
    const result = await bridge.call<{ id: string }>("doc.version", checked.input);
    if (!result.ok) state.notice = bad(result.text);
    else {
      versionName.value = "";
      state.notice = { text: readable(result.text) || "Version named.", tone: "ok" };
      await refreshDoc();
    }
  } catch (error) {
    state.notice = { text: failure(error, "Naming a version"), tone: "bad" };
  } finally {
    state.busy = false;
    render();
  }
}

// --- Dialogs ------------------------------------------------------------------------

function dialog(label: string, test: string, build: (close: () => void) => Node[], onClose?: () => void): () => void {
  root.querySelector(".sheet-backdrop")?.remove();
  const close = () => {
    backdrop.remove();
    document.removeEventListener("keydown", escape);
    onClose?.();
  };
  const escape = (e: KeyboardEvent) => {
    if (e.key === "Escape") close();
  };
  document.addEventListener("keydown", escape);
  const sheet = h("div", { class: "sheet", role: "dialog", "aria-modal": "true", "aria-label": label, "data-test": test }, ...build(close));
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
  sheet.querySelector<HTMLElement>("input, select, textarea")?.focus();
  return close;
}

const field = (label: string, ...children: (Node | string | null)[]) => h("div", { class: "field" }, h("span", { class: "field-label" }, label), ...children);

function roleSelect(value: Share, onchange: (share: Share) => void, label: string): HTMLSelectElement {
  const select = h(
    "select",
    { class: "role-select", "aria-label": label, onchange: (e: Event) => onchange((e.target as HTMLSelectElement).value as Share) },
    ...(["editor", "commenter", "viewer"] as const).map((s) => {
      const option = h("option", { value: s }, roleName(s));
      option.selected = s === value;
      return option;
    }),
  );
  return select;
}

/** Someone being added: who, their email when the directory has it, and what they may do. */
interface Picked extends Found {
  share: Share;
}

/**
 * People as they're typed (AD-1): the org's directory through
 * people.search. Picking one fills in who they are and shows their email.
 */
function peopleField(picked: Picked[], onchange: () => void): HTMLElement {
  let found: Found[] = [];
  let query = "";
  let highlight = 0;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const chips = h("div", { class: "invitees", "data-test": "invitees" });
  const suggestions = h("div", { class: "suggestions", role: "listbox", hidden: true, "data-test": "suggestions" });
  const input = h("input", {
    name: "people",
    autocomplete: "off",
    placeholder: "Type a name",
    "aria-label": "People",
    role: "combobox",
    "aria-autocomplete": "list",
    oninput: (e: Event) => {
      query = (e.target as HTMLInputElement).value;
      clearTimeout(timer);
      timer = setTimeout(() => void search(query), 120);
    },
    onkeydown: (e: Event) => {
      const key = e as KeyboardEvent;
      if (!found.length) return;
      if (key.key === "ArrowDown" || key.key === "ArrowUp") {
        e.preventDefault();
        highlight = (highlight + (key.key === "ArrowDown" ? 1 : -1) + found.length) % found.length;
        draw();
      } else if (key.key === "Enter" || key.key === "Tab") {
        const person = found[highlight];
        if (person) {
          e.preventDefault();
          add(person);
        }
      } else if (key.key === "Escape") {
        e.stopPropagation();
        found = [];
        draw();
      }
    },
  });
  const add = (person: Found) => {
    picked.push({ ...person, share: "editor" });
    found = [];
    query = "";
    input.value = "";
    draw();
    onchange();
    input.focus();
  };
  const draw = () => {
    show(
      chips,
      ...picked.map((p) =>
        h(
          "div",
          { class: "invitee", "data-person": p.login },
          h("span", { class: "avatar small" }, initials(p.name)),
          h("span", { class: "invitee-who" }, h("span", { class: "who" }, p.name), h("span", { class: "muted small" }, p.email ?? p.login)),
          roleSelect(p.share, (share) => (p.share = share), `What ${p.name} may do`),
          h(
            "button",
            {
              type: "button",
              class: "chip-x",
              "aria-label": `Remove ${p.name}`,
              onclick: () => {
                picked.splice(picked.indexOf(p), 1);
                draw();
                onchange();
              },
            },
            "×",
          ),
        ),
      ),
    );
    suggestions.hidden = found.length === 0;
    show(
      suggestions,
      ...found.map((p, i) =>
        h(
          "button",
          {
            type: "button",
            class: `suggestion${i === highlight ? " on" : ""}`,
            role: "option",
            "data-login": p.login,
            onmousedown: (e: Event) => e.preventDefault(),
            onclick: () => add(p),
          },
          h("span", { class: "who" }, p.name),
          h("span", { class: "address" }, p.email ?? p.login),
          p.title ? h("span", { class: "about" }, p.title) : null,
        ),
      ),
    );
  };
  async function search(q: string): Promise<void> {
    if (!q.trim()) {
      found = [];
      return draw();
    }
    try {
      const result = await bridge.call<{ people: Found[] }>("people.search", { query: q, limit: 8 });
      if (query !== q) return;
      found = result.ok ? result.data.people.filter((f) => f.login !== state.me && !picked.some((x) => x.login === f.login)) : [];
    } catch {
      found = [];
    }
    highlight = 0;
    draw();
  }
  draw();
  return h("div", { class: "people-field" }, chips, h("div", { class: "address-field" }, input, suggestions));
}

/** A new doc: its title, who's in it and what each may do, and its label (DC-4). */
function openCreate(): void {
  const picked: Picked[] = [];
  let busy = false;
  const error = h("div", { class: "form-error", role: "alert", hidden: true });
  const say = (text: string | null) => {
    error.hidden = !text;
    show(error, text ?? "");
  };
  const title = h("input", { name: "title", autocomplete: "off", maxlength: MAX_TITLE_CHARS, placeholder: "Q3 plan", "aria-label": "Title" });
  const label = h(
    "select",
    { name: "label", "aria-label": "Label" },
    ...DOC_LABELS.map((l) => {
      const option = h("option", { value: l }, labelName(l) ?? l);
      option.selected = l === "internal";
      return option;
    }),
  );
  const asks = bridge.asks("doc.create");
  const submit = h("button", { class: "button primary", type: "submit", "data-test": "dialog-submit" }, "Create");
  dialog("New doc", "create-dialog", (close) => [
    h(
      "form",
      {
        class: "sheet-form",
        onsubmit: (e: Event) => {
          e.preventDefault();
          void send(close);
        },
      },
      h("h2", {}, "New doc"),
      field("Title", title),
      field("With", peopleField(picked, () => say(null))),
      h("p", { class: "muted small sheet-note" }, "Who's in a doc is set when it's made. You can change what each person may do later."),
      field("Label", label, h("span", { class: "muted small" }, "Who may see it, and what may read it.")),
      error,
      h(
        "div",
        { class: "sheet-actions" },
        asks ? h("span", { class: "muted small" }, "Your organization asks you to approve this on the desktop.") : h("span", {}),
        h("button", { class: "button", type: "button", onclick: () => close() }, "Cancel"),
        submit,
      ),
    ),
  ]);

  async function send(close: () => void): Promise<void> {
    if (busy) return;
    const checked = createInput({
      title: title.value,
      label: label.value as Label,
      people: picked.map((p): Invitee => ({ login: p.login, share: p.share })),
    });
    if ("problem" in checked) return say(checked.problem);
    busy = true;
    submit.disabled = true;
    show(submit, asks ? "Waiting for your approval on the desktop…" : "Creating…");
    try {
      const result = await bridge.call<{ id: string }>("doc.create", checked.input);
      if (!result.ok) return say(readable(result.text));
      close();
      await loadList();
      await openDoc(result.data.id);
    } catch (error) {
      say(failure(error, "Creating it"));
    } finally {
      busy = false;
      submit.disabled = false;
      show(submit, "Create");
    }
  }
}

/** Who's in the open doc and what each may do; the owner changes roles (doc.roles). */
function openShare(): void {
  const open = state.open;
  const meta = open?.meta;
  if (!open || !meta) return;
  const owner = meta.role === "owner";
  const people: { login: string; name: string; role: Role }[] = meta.people.map((p) => ({ login: p.login, name: p.name, role: p.role ?? "viewer" }));
  if (!people.some((p) => p.login === meta.owner)) people.unshift({ login: meta.owner, name: meta.owner_name, role: "owner" });
  let busy = false;
  const error = h("div", { class: "form-error", role: "alert", hidden: true });
  const submit = h("button", { class: "button primary", type: "submit", "data-test": "dialog-submit" }, "Save");
  dialog(`Share ${meta.title}`, "share-dialog", (close) => [
    h(
      "form",
      {
        class: "sheet-form",
        onsubmit: (e: Event) => {
          e.preventDefault();
          void save(close);
        },
      },
      h("h2", {}, `Share “${meta.title}”`),
      h(
        "p",
        { class: "muted sheet-about" },
        owner ? "Who's in a doc is set when it's made. You can change what each person may do." : "Who's in this doc, and what each may do. Its owner can change that.",
      ),
      h(
        "div",
        { class: "people-list", "data-test": "people" },
        ...people.map((p) =>
          h(
            "div",
            { class: "invitee", "data-person": p.login },
            h("span", { class: "avatar small" }, initials(p.name)),
            h("span", { class: "invitee-who" }, h("span", { class: "who" }, p.login === state.me ? `${p.name} (you)` : p.name), h("span", { class: "muted small" }, p.login)),
            p.role === "owner" || !owner ? h("span", { class: "role-tag" }, roleName(p.role)) : roleSelect(p.role as Share, (share) => (p.role = share), `What ${p.name} may do`),
          ),
        ),
      ),
      field("Label", h("span", { class: "label-line" }, labelTag(meta.label) ?? h("span", { class: "muted" }, "None"), h("span", { class: "muted small" }, "Set when the doc was made."))),
      error,
      h(
        "div",
        { class: "sheet-actions" },
        h("span", {}),
        h("button", { class: "button", type: "button", onclick: () => close() }, owner ? "Cancel" : "Close"),
        owner ? submit : null,
      ),
    ),
  ]);

  async function save(close: () => void): Promise<void> {
    if (busy || !open || !meta) return;
    busy = true;
    submit.disabled = true;
    try {
      const result = await bridge.call("doc.roles", rolesInput(open.id, meta.owner, people));
      if (!result.ok) {
        error.hidden = false;
        show(error, readable(result.text));
        return;
      }
      close();
      state.notice = { text: readable(result.text) || "Roles saved.", tone: "ok" };
      await refreshDoc();
      render();
    } catch (e) {
      error.hidden = false;
      show(error, failure(e, "Saving roles"));
    } finally {
      busy = false;
      submit.disabled = false;
    }
  }
}

/** "Link to a doc": one of the person's docs, by title. */
function pickDoc(): Promise<{ id: string; title: string } | null> {
  return new Promise((done) => {
    let chosen: { id: string; title: string } | null = null;
    const list = h("div", { class: "pick-list", "data-test": "pick-docs" });
    const filter = h("input", {
      name: "filter",
      autocomplete: "off",
      placeholder: "Find a doc",
      "aria-label": "Find a doc",
      oninput: () => draw(),
    });
    let close = () => {};
    const draw = () => {
      const q = filter.value.trim().toLowerCase();
      const rows = state.rows.filter((r) => r.id !== state.open?.id && (!q || r.title.toLowerCase().includes(q)));
      show(
        list,
        ...(rows.length
          ? rows.slice(0, 50).map((r) =>
              h(
                "button",
                {
                  type: "button",
                  class: "row",
                  "data-doc": r.id,
                  onclick: () => {
                    chosen = { id: r.id, title: r.title };
                    close();
                  },
                },
                h("span", { class: "doc-icon small", "aria-hidden": "true" }),
                h("span", { class: "row-main" }, h("span", { class: "row-title" }, r.title), h("span", { class: "row-meta" }, r.mine ? "Yours" : r.owner_name)),
              ),
            )
          : [h("div", { class: "side-empty" }, state.rows.length ? "No doc by that name" : "No other docs yet")]),
      );
    };
    draw();
    close = dialog("Link to a doc", "pick-doc", (c) => [h("h2", {}, "Link to a doc"), filter, list, h("div", { class: "sheet-actions" }, h("span", {}), h("button", { class: "button", type: "button", onclick: () => c() }, "Cancel"))], () => done(chosen));
  });
}

/** "Artifact": a card for one, from its link in Artifacts (the app can't list them). */
function pickArtifact(): Promise<{ id: string; title: string } | null> {
  return new Promise((done) => {
    let chosen: { id: string; title: string } | null = null;
    const error = h("div", { class: "form-error", role: "alert", hidden: true });
    const link = h("input", { name: "artifact", autocomplete: "off", spellcheck: "false", placeholder: "http://artifacts.localhost/#cv-…", "aria-label": "Artifact link" });
    const title = h("input", { name: "title", autocomplete: "off", maxlength: MAX_TITLE_CHARS, placeholder: "Q3 forecast", "aria-label": "Title" });
    dialog(
      "Embed an artifact",
      "pick-artifact",
      (close) => [
        h(
          "form",
          {
            class: "sheet-form",
            onsubmit: (e: Event) => {
              e.preventDefault();
              const id = artifactFrom(link.value);
              if (!id) {
                error.hidden = false;
                show(error, "Paste an artifact's link from Artifacts (it ends in #cv-…).");
                return;
              }
              chosen = { id, title: title.value.trim() || "Artifact" };
              close();
            },
          },
          h("h2", {}, "Embed an artifact"),
          h("p", { class: "muted sheet-about" }, "It shows as a card that opens the artifact in Artifacts, for whoever may see it there."),
          field("Link", link),
          field("Title", title),
          error,
          h("div", { class: "sheet-actions" }, h("span", {}), h("button", { class: "button", type: "button", onclick: () => close() }, "Cancel"), h("button", { class: "button primary", type: "submit", "data-test": "dialog-submit" }, "Embed")),
        ),
      ],
      () => done(chosen),
    );
  });
}

// --- Drawing ------------------------------------------------------------------------------

/** The data label's badge (DP): Public, Internal, Confidential. */
function labelTag(label: unknown): HTMLElement | null {
  const name = labelName(label);
  return name ? h("span", { class: `label-tag ${String(label)}`, "data-test": "label", title: `Labeled ${name}` }, name) : null;
}

function rowView(r: Row): HTMLElement {
  const active = state.open?.id === r.id;
  return h(
    "button",
    { class: `row${active ? " active" : ""}`, "data-doc": r.id, onclick: () => void openDoc(r.id) },
    h("span", { class: "doc-icon small", "aria-hidden": "true" }),
    h(
      "span",
      { class: "row-main" },
      h("span", { class: "row-title" }, r.title || "Untitled"),
      h(
        "span",
        { class: "row-meta" },
        labelTag(r.label),
        r.mine ? null : h("span", { class: `role-tag ${r.role}`, "data-test": "role" }, roleName(r.role)),
        h("span", { class: "row-sub" }, [r.mine ? null : r.owner_name, when(r.updated_at)].filter(Boolean).join(" · ")),
      ),
    ),
  );
}

function renderSidebar(): void {
  const { shared, mine } = partition(state.rows);
  show(
    sidebar,
    h("div", { class: "side-head" }, h("h1", {}, "Docs"), h("button", { class: "button small", "data-test": "new-doc", onclick: () => openCreate() }, "New doc")),
    h(
      "section",
      { class: "side-section", "data-test": "shared-with-me" },
      h("h2", {}, "Shared with me"),
      ...(shared.length ? shared.map(rowView) : [h("div", { class: "side-empty" }, state.loaded ? "Nothing shared with you yet" : "")]),
    ),
    h(
      "section",
      { class: "side-section", "data-test": "mine" },
      h("h2", {}, "Mine"),
      ...(mine.length ? mine.map(rowView) : [h("div", { class: "side-empty" }, state.loaded ? "Docs you make show here" : "")]),
    ),
    h("footer", { class: "about" }, "Edited live with the people in each doc. Your agent can read your docs, and writes in them only with your yes."),
  );
}

function renderWelcome(): void {
  show(
    welcome,
    h(
      "div",
      { class: "empty welcome", "data-test": "welcome" },
      h("span", { class: "doc-icon large", "aria-hidden": "true" }),
      h("h2", {}, state.rows.length ? "Pick a doc" : "No docs yet"),
      h(
        "p",
        { class: "muted" },
        "Write with your colleagues at the same time: headings, lists, checklists, tables, code, callouts, links between docs and artifacts. Press / in a doc for every kind of block.",
      ),
      h("button", { class: "button primary", onclick: () => openCreate() }, "New doc"),
    ),
  );
}

/** What the page says about saving: caught up, sending, can't reach the system, or refused. */
function syncState(open: Open): { text: string; tone: string } {
  if (!open.meta) return { text: "Opening…", tone: "opening" };
  if (open.health === "offline") return { text: "Offline · editing paused", tone: "offline" };
  if (open.health === "refused") return { text: open.why ? `Not saved: ${open.why}` : "Not saved", tone: "refused" };
  if (!canEdit(open.meta.role)) return { text: "Live", tone: "live" };
  if (open.health === "sending") return { text: "Saving…", tone: "sending" };
  return { text: "Saved", tone: "live" };
}

function avatars(people: Person[]): HTMLElement {
  const shown = people.slice(0, 4);
  return h(
    "span",
    { class: "avatars", title: people.map((p) => `${p.name} · ${roleName(p.role)}`).join("\n") },
    ...shown.map((p) => h("span", { class: "avatar round" }, initials(p.name))),
    people.length > shown.length ? h("span", { class: "avatar round more" }, `+${people.length - shown.length}`) : null,
  );
}

function renderHead(): void {
  const open = state.open;
  if (!open) {
    show(head, h("h1", {}, "Docs"));
    return;
  }
  const meta = open.meta;
  const row = state.rows.find((r) => r.id === open.id);
  const role = meta?.role ?? row?.role;
  const sync = syncState(open);
  const comments = meta?.comments.filter((c) => !c.reply_to && !c.resolved).length ?? 0;
  const toggle = (which: "comments" | "versions", label: string, test: string) =>
    h(
      "button",
      {
        class: `button toggle${state.panel === which ? " on" : ""}`,
        type: "button",
        "aria-pressed": String(state.panel === which),
        "data-test": test,
        onclick: () => {
          state.panel = state.panel === which ? null : which;
          render();
        },
      },
      label,
    );
  show(
    head,
    h(
      "div",
      { class: "head-title" },
      h("h1", { "data-test": "title" }, meta?.title ?? row?.title ?? "Doc"),
      labelTag(meta?.label ?? row?.label),
      role && !canEdit(role) ? h("span", { class: `role-tag ${role}`, "data-test": "role" }, role === "commenter" ? "You can comment" : "View only") : null,
      h("span", { class: `sync ${sync.tone}`, "data-test": "sync", role: "status" }, h("span", { class: "sync-dot", "aria-hidden": "true" }), sync.text),
    ),
    h(
      "div",
      { class: "head-actions" },
      meta ? avatars(meta.people.length ? meta.people : [{ login: meta.owner, name: meta.owner_name, role: "owner" }]) : null,
      toggle("comments", comments ? `Comments · ${comments}` : "Comments", "comments-toggle"),
      toggle("versions", "Versions", "versions-toggle"),
      h("button", { class: "button primary", type: "button", "data-test": "share", onclick: () => openShare() }, "Share"),
    ),
  );
}

function renderBanners(): void {
  show(
    banners,
    state.notice && h("div", { class: `banner ${state.notice.tone === "ok" ? "done" : "refused"}`, role: "status", "data-test": "notice" }, state.notice.text),
    state.offline && h("div", { class: "banner offline" }, "Can't reach the system right now. Edits pause until it's back."),
  );
}

function renderDocTop(): void {
  const open = state.open;
  const meta = open?.meta;
  const row = state.rows.find((r) => r.id === open?.id);
  const owner = meta ? (meta.owner === state.me ? "You" : meta.owner_name) : row ? (row.mine ? "You" : row.owner_name) : "";
  const others = (meta?.people ?? []).filter((p) => p.login !== meta?.owner && p.login !== state.me).map((p) => p.name);
  const touched = open?.touched && Date.now() - open.touched.at < 5 * 60_000 ? `${open.touched.by_name} edited just now` : null;
  show(
    docTop,
    h("h1", { class: "doc-title", "data-test": "doc-title" }, meta?.title ?? row?.title ?? ""),
    h(
      "div",
      { class: "doc-meta" },
      owner ? h("span", {}, owner === "You" ? "Yours" : `${owner}'s`) : null,
      others.length ? h("span", {}, `with ${names(others)}`) : null,
      touched ? h("span", { class: "touched" }, touched) : row?.updated_at ? h("span", {}, `Updated ${when(row.updated_at)}`) : null,
    ),
  );
}

function commentView(c: Comment, isReply: boolean): HTMLElement {
  const by = parseAttribution(c.attribution);
  const open = state.open;
  const may = canComment(open?.meta?.role);
  return h(
    "div",
    { class: `comment${isReply ? " reply" : ""}`, "data-comment": c.id },
    h(
      "div",
      { class: "comment-head" },
      h("span", { class: "avatar small" }, initials(c.by_name)),
      h("span", { class: "who" }, c.by === state.me ? `${c.by_name} (you)` : c.by_name),
      h("span", { class: "time" }, when(c.at)),
    ),
    by && showAttribution(by, "docs") ? h("div", { class: "comment-by" }, attributionChip(by.text)) : null,
    h("p", { class: "comment-text" }, c.text),
    isReply || !may
      ? null
      : h(
          "div",
          { class: "comment-actions" },
          h(
            "button",
            {
              class: "link",
              type: "button",
              onclick: (e: Event) => {
                e.stopPropagation();
                state.replyTo = state.replyTo === c.id ? null : c.id;
                state.replyDraft = "";
                renderPanel();
                panelBody.querySelector<HTMLTextAreaElement>("[name=reply]")?.focus();
              },
            },
            "Reply",
          ),
          h(
            "button",
            {
              class: "link",
              type: "button",
              "data-test": c.resolved ? "reopen" : "resolve",
              onclick: (e: Event) => {
                e.stopPropagation();
                void resolve(c);
              },
            },
            c.resolved ? "Reopen" : "Resolve",
          ),
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
      onclick: (e: Event) => e.stopPropagation(),
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
            renderPanel();
          },
        },
        "Cancel",
      ),
      h("button", { class: "button primary small", type: "submit" }, "Reply"),
    ),
  );
}

/** A thread's quote of the text it's about, found at its anchor now. */
function quote(c: Comment): HTMLElement | null {
  if (!c.anchor?.path) return h("div", { class: "thread-quote whole" }, "On the whole doc");
  const text = state.open?.editor?.quoteOf(c.anchor.path);
  return text ? h("div", { class: "thread-quote" }, text) : h("div", { class: "thread-quote gone" }, "The text it was on has changed");
}

function renderComments(): void {
  const open = state.open;
  const meta = open?.meta;
  const focused = document.activeElement instanceof HTMLTextAreaElement && panelBody.contains(document.activeElement) && document.activeElement.name === "reply";
  if (!meta) {
    show(panelHead, h("h2", {}, "Comments"));
    show(panelBody, h("div", { class: "comments-empty" }, "Opening…"));
    show(panelFoot);
    return;
  }
  const all = threads(meta.comments);
  const openThreads = all.filter((t) => !t.comment.resolved);
  const resolved = all.filter((t) => t.comment.resolved);
  const may = canComment(meta.role);
  show(
    panelHead,
    h("h2", {}, "Comments"),
    h("span", { class: "muted" }, openThreads.length ? `${openThreads.length} open` : meta.comments.length ? "All resolved" : ""),
    h("button", { class: "chip-x", type: "button", "aria-label": "Close comments", onclick: () => ((state.panel = null), render()) }, "×"),
  );
  const thread = (t: { comment: Comment; replies: Comment[] }) =>
    h(
      "article",
      {
        class: `thread${t.comment.resolved ? " resolved" : ""}${state.active === t.comment.id ? " active" : ""}`,
        "data-thread": t.comment.id,
        onclick: () => focusThread(t.comment.id, true),
      },
      quote(t.comment),
      commentView(t.comment, false),
      ...t.replies.map((r) => commentView(r, true)),
      state.replyTo === t.comment.id ? replyBox(t.comment.id) : null,
    );
  const draft = state.draft
    ? h(
        "form",
        {
          class: "draft",
          "data-test": "composer",
          onsubmit: (e: Event) => {
            e.preventDefault();
            void postDraft();
          },
        },
        h("div", { class: "thread-quote" }, state.draft.quote || "Here"),
        draftText,
        h(
          "div",
          { class: "composer-row" },
          h("span", { class: "composer-hint" }, bridge.asks("doc.comment") ? "Waits for your approval on the desktop" : "Ctrl+Enter to post"),
          h(
            "button",
            {
              class: "button small",
              type: "button",
              onclick: () => {
                state.draft = null;
                renderPanel();
              },
            },
            "Cancel",
          ),
          h("button", { class: "button primary small", type: "submit", disabled: state.busy, "data-test": "post-comment" }, "Comment"),
        ),
      )
    : null;
  show(
    panelBody,
    draft,
    ...(openThreads.length ? openThreads.map(thread) : draft ? [] : [h("div", { class: "comments-empty" }, may ? "No comments yet. Select text and choose Comment." : "No comments yet.")]),
    resolved.length
      ? h(
          "button",
          { class: "link resolved-toggle", type: "button", "data-test": "show-resolved", onclick: () => ((state.showResolved = !state.showResolved), renderPanel()) },
          state.showResolved ? `Hide resolved (${resolved.length})` : `Show resolved (${resolved.length})`,
        )
      : null,
    ...(state.showResolved ? resolved.map(thread) : []),
  );
  if (may) {
    show(
      panelFoot,
      h(
        "form",
        {
          class: "composer",
          onsubmit: (e: Event) => {
            e.preventDefault();
            void postGeneral();
          },
        },
        generalText,
        h("div", { class: "composer-row" }, h("span", { class: "composer-hint" }, "Ctrl+Enter to post"), h("button", { class: "button small", type: "submit", disabled: state.busy }, "Comment")),
      ),
    );
  } else {
    show(panelFoot, h("div", { class: "panel-note" }, "You can view this doc. Its owner can let you comment."));
  }
  if (focused) {
    const box = panelBody.querySelector<HTMLTextAreaElement>("[name=reply]");
    box?.focus();
    box?.setSelectionRange(box.value.length, box.value.length);
  }
}

function renderVersions(): void {
  const meta = state.open?.meta;
  show(
    panelHead,
    h("h2", {}, "Versions"),
    h("span", { class: "muted" }, meta?.versions.length ? `${meta.versions.length} named` : ""),
    h("button", { class: "chip-x", type: "button", "aria-label": "Close versions", onclick: () => ((state.panel = null), render()) }, "×"),
  );
  const versions = [...(meta?.versions ?? [])].sort((a, b) => b.at.localeCompare(a.at));
  show(
    panelBody,
    canEdit(meta?.role)
      ? h(
          "form",
          {
            class: "version-form",
            "data-test": "name-version",
            onsubmit: (e: Event) => {
              e.preventDefault();
              void nameVersion();
            },
          },
          h("span", { class: "field-label" }, "Name this version"),
          h("div", { class: "version-row" }, versionName, h("button", { class: "button small primary", type: "submit", disabled: state.busy }, "Save")),
          h("span", { class: "muted small" }, "Everyone in the doc sees it in this list."),
        )
      : null,
    h(
      "ol",
      { class: "versions", "data-test": "versions" },
      h("li", { class: "version now" }, h("span", { class: "version-dot", "aria-hidden": "true" }), h("span", { class: "version-main" }, h("span", { class: "version-name" }, "Now"), h("span", { class: "muted small" }, "What everyone sees"))),
      ...versions.map((v) =>
        h(
          "li",
          { class: "version", "data-version": v.id },
          h("span", { class: "version-dot", "aria-hidden": "true" }),
          h("span", { class: "version-main" }, h("span", { class: "version-name" }, v.name), h("span", { class: "muted small" }, `${v.by_name} · ${when(v.at)}`)),
        ),
      ),
    ),
    versions.length ? null : h("div", { class: "comments-empty" }, "No named versions yet."),
  );
  show(panelFoot);
}

function renderPanel(): void {
  panel.hidden = !state.panel || !state.open;
  docView.classList.toggle("with-panel", !panel.hidden);
  if (state.panel === "comments") renderComments();
  else if (state.panel === "versions") renderVersions();
}

function render(): void {
  renderSidebar();
  renderHead();
  renderBanners();
  const open = Boolean(state.open);
  welcome.hidden = open;
  docView.hidden = !open;
  if (!open) renderWelcome();
  else renderDocTop();
  renderPanel();
}

/** Waits for news and reads it: the list, and the open doc's comments and versions. */
async function news(): Promise<void> {
  for (;;) {
    try {
      const status = await bridge.status({ waitSecs: 25, since: state.cursor });
      state.cursor = status.cursor;
      if (status.changed.includes("docs")) soon();
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

export async function mount(into: HTMLElement, with_: Bridge): Promise<void> {
  root = into;
  bridge = with_;
  show(root, sidebar, h("main", { class: "pane" }, head, banners, body));
  window.addEventListener("hashchange", () => {
    const id = deepLink(location.hash);
    if (id && id !== state.open?.id) void openDoc(id);
  });
  window.addEventListener("pagehide", () => void state.open?.live?.flush());
  try {
    const session = await bridge.open();
    state.me = session.user;
  } catch (error) {
    state.notice = { text: failure(error, "Docs"), tone: "bad" };
  }
  // The cursor before the first load, so a change in between is news
  // (NV-3, ADR 0051).
  try {
    state.cursor = (await bridge.status()).cursor;
  } catch {
    // news() says so.
  }
  await loadList();
  const linked = deepLink(location.hash);
  if (linked) await openDoc(linked);
  else render();
  ready();
  void news();
}
