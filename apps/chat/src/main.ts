// Chat (CF-91, ADR 0030): channels and direct messages with people in the
// org, sealed end to end between their computers. glassd seals and opens
// everything; this page shows what it is given and keeps nothing: every
// call is `bridge.call`, never `bridge.read`, so no message lands in the
// page's storage. Live updates come from `app.status` waits, which carry
// ids only, so the audit log gets one read per change rather than per poll.

import {
  attributionChip,
  Bridge,
  BridgeError,
  h,
  Offline,
  parseAttribution,
  ready,
  show,
  showAttribution,
} from "@glasshouse/sdk";

type Kind = "direct" | "group" | "channel";

interface Person {
  login: string;
  name: string;
}

interface Conversation {
  id: string;
  kind: Kind;
  title: string;
  people: Person[];
  unread: number;
  last_at: string | null;
  last_from: string | null;
}

/** Something shared into the conversation (ADR 0033): an artifact shows as a card. */
interface Shared {
  item: string;
  kind: string;
  title: string;
}

interface Message {
  id: string;
  seq: number;
  sender: string;
  sender_name: string;
  mine: boolean;
  at: string;
  text: string;
  attribution: string | null;
  shared?: Shared | null;
}

interface Found {
  login: string;
  name: string;
  email: string;
  title?: string;
}

/** Choosing people: for a new message, or a new channel's members. */
interface Picker {
  query: string;
  found: Found[];
  picked: Found[];
}

type Mode = { kind: "conversation"; id: string } | { kind: "new-message"; to: Picker } | { kind: "new-channel"; name: string; to: Picker } | { kind: "none" };

const bridge = new Bridge({ app: "chat" });
const state = {
  me: "",
  conversations: [] as Conversation[],
  mode: { kind: "none" } as Mode,
  messages: [] as Message[],
  /** What is being sent now, shown until it is posted or refused. */
  sending: null as { text: string; waiting: string | null } | null,
  notice: null as { text: string; tone: "bad" | "ok" } | null,
  fingerprint: null as string | null,
  offline: false,
  cursor: 0,
};

const root = document.getElementById("app") as HTMLElement;
// The parts that live updates redraw; the composer stays, so typing is
// never interrupted.
const sidebar = h("aside", { class: "sidebar" });
const head = h("header", { class: "pane-head" });
const banner = h("div", { class: "banners" });
const body = h("div", { class: "pane-body" });
const text = h("textarea", {
  class: "composer-text",
  name: "message",
  rows: 1,
  "aria-label": "Message",
  onkeydown: (e: Event) => {
    const key = e as KeyboardEvent;
    if (key.key === "Enter" && !key.shiftKey && !key.isComposing) {
      e.preventDefault();
      void send();
    }
  },
  oninput: () => grow(),
});
const hint = h("div", { class: "composer-hint" });
const composer = h(
  "form",
  {
    class: "composer",
    "data-test": "composer",
    onsubmit: (e: Event) => {
      e.preventDefault();
      void send();
    },
  },
  text,
  h("button", { class: "button primary", type: "submit" }, "Send"),
);

function grow(): void {
  text.style.height = "auto";
  text.style.height = `${Math.min(text.scrollHeight, 180)}px`;
}

function failure(error: unknown, doing: string): string {
  if (error instanceof Offline) return `${doing} needs the system, and it isn't answering.`;
  if (error instanceof BridgeError) return error.message;
  return String(error);
}

function openId(): string | null {
  return state.mode.kind === "conversation" ? state.mode.id : null;
}

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

async function loadMessages(): Promise<void> {
  const id = openId();
  if (!id) return;
  try {
    const result = await bridge.call<{ messages: Message[] }>("chat.read", { conversation: id });
    if (openId() !== id) return;
    if (result.ok) {
      state.messages = result.data.messages;
      // Reading it in Chat marked it read.
      const row = state.conversations.find((c) => c.id === id);
      if (row) row.unread = 0;
    } else state.notice = { text: result.text, tone: "bad" };
  } catch (error) {
    state.notice = { text: failure(error, "Reading"), tone: "bad" };
  }
}

async function open(id: string): Promise<void> {
  state.mode = { kind: "conversation", id };
  state.messages = [];
  state.notice = null;
  render();
  await loadMessages();
  render();
  text.focus();
}

function picker(): Picker {
  return { query: "", found: [], picked: [] };
}

async function search(p: Picker, query: string): Promise<void> {
  p.query = query;
  if (!query.trim()) {
    p.found = [];
    return renderBody();
  }
  try {
    const result = await bridge.call<{ people: Found[] }>("people.search", { query, limit: 8 });
    if (p.query !== query) return;
    p.found = result.ok ? result.data.people.filter((f) => f.login !== state.me && !p.picked.some((x) => x.login === f.login)) : [];
  } catch {
    p.found = [];
  }
  renderBody();
}

/** Sends what's in the composer: to the open conversation, or to the people picked. */
async function send(): Promise<void> {
  const message = text.value;
  if (!message.trim() || state.sending) return;
  const mode = state.mode;
  let post: Record<string, unknown>;
  if (mode.kind === "conversation") post = { conversation: mode.id, text: message };
  else if (mode.kind === "new-message" && mode.to.picked.length) post = { to: mode.to.picked.map((p) => p.login), text: message };
  else return;
  state.sending = { text: message, waiting: bridge.asks("chat.post") ? "Waiting for your approval on the desktop" : null };
  state.notice = null;
  text.value = "";
  grow();
  render();
  // While the desktop asks, show what it asks (app.status).
  const watch = setInterval(() => {
    bridge
      .status()
      .then((status) => {
        const asked = status.waiting ? `Waiting for your approval on the desktop: ${status.waiting.summary}` : null;
        if (state.sending && asked !== state.sending.waiting && (asked || !bridge.asks("chat.post"))) {
          state.sending.waiting = asked;
          renderBody();
        }
      })
      .catch(() => undefined);
  }, 400);
  try {
    const result = await bridge.call<{ posted: { conversation: string }[] }>("chat.post", { posts: [post] });
    if (!result.ok) {
      // Not sent: the words stay, so nothing typed is lost.
      text.value = message;
      grow();
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
    }
  } catch (error) {
    text.value = message;
    grow();
    state.notice = { text: `Not sent: ${failure(error, "Sending")}`, tone: "bad" };
  } finally {
    clearInterval(watch);
    state.sending = null;
  }
  render();
  text.focus();
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

/** Waits for news and reads it: one `chat.list`, and the open conversation, per change. */
async function live(): Promise<void> {
  for (;;) {
    try {
      const status = await bridge.status({ waitSecs: 25, since: state.cursor });
      state.cursor = status.cursor;
      if (status.changed.includes("chat") && !state.sending) {
        await loadList();
        await loadMessages();
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

function initials(name: string): string {
  const parts = name.trim().split(/\s+/);
  return ((parts[0]?.[0] ?? "") + (parts.length > 1 ? (parts[parts.length - 1]?.[0] ?? "") : "")).toUpperCase() || "?";
}

function time(at: string): string {
  const d = new Date(at);
  return Number.isNaN(d.getTime()) ? "" : d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
}

function row(c: Conversation): HTMLElement {
  const active = openId() === c.id;
  return h(
    "button",
    {
      class: `row${active ? " active" : ""}${c.unread ? " unread" : ""}`,
      "data-conversation": c.id,
      onclick: () => void open(c.id),
    },
    h("span", { class: "row-mark" }, c.kind === "channel" ? "#" : c.kind === "group" ? String(c.people.length - 1) : initials(c.title)),
    h("span", { class: "row-title" }, c.kind === "channel" ? c.title.replace(/^#/, "") : c.title),
    c.unread ? h("span", { class: "badge", "aria-label": `${c.unread} unread` }, String(c.unread)) : null,
  );
}

function renderSidebar(): void {
  const channels = state.conversations.filter((c) => c.kind === "channel");
  const people = state.conversations.filter((c) => c.kind !== "channel");
  show(
    sidebar,
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
            state.mode = { kind: "new-message", to: picker() };
            render();
            document.querySelector<HTMLInputElement>("[name=to]")?.focus();
          },
        },
        "✎",
      ),
    ),
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
              state.mode = { kind: "new-channel", name: "", to: picker() };
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
                renderBody();
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
                  renderBody();
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

function messageView(m: Message, previous: Message | undefined): HTMLElement {
  const by = parseAttribution(m.attribution);
  const agent = showAttribution(by, "chat");
  // Messages from the same person close together read as one block.
  const joined = previous && previous.sender === m.sender && !agent && !previous.attribution;
  return h(
    "div",
    { class: `message${joined ? " joined" : ""}`, "data-message": m.id },
    joined ? h("span", { class: "avatar-space" }) : h("span", { class: "avatar" }, initials(m.sender_name)),
    h(
      "div",
      { class: "message-body" },
      joined
        ? null
        : h(
            "div",
            { class: "message-head" },
            h("span", { class: "sender" }, m.mine ? `${m.sender_name} (you)` : m.sender_name),
            h("span", { class: "time" }, time(m.at)),
            agent && by ? attributionChip(by.text) : null,
          ),
      m.shared?.kind === "artifact" ? artifactCard(m.shared) : h("div", { class: "message-text" }, m.text),
    ),
  );
}

/** Draws the pane's body, keeping the focus in the field that had it. */
function renderBody(): void {
  const focused = document.activeElement instanceof HTMLInputElement && body.contains(document.activeElement) ? document.activeElement.name : null;
  drawBody();
  renderComposer();
  if (focused) {
    const field = body.querySelector<HTMLInputElement>(`input[name=${focused}]`);
    field?.focus();
    field?.setSelectionRange(field.value.length, field.value.length);
  }
}

/** The composer shows when there is somewhere to send. */
function renderComposer(): void {
  const mode = state.mode;
  composer.hidden = !(mode.kind === "conversation" || (mode.kind === "new-message" && mode.to.picked.length > 0));
  const c = mode.kind === "conversation" ? state.conversations.find((x) => x.id === mode.id) : undefined;
  text.placeholder = c ? `Message ${c.title}` : mode.kind === "new-message" ? `Message ${mode.to.picked.map((p) => p.name).join(", ")}` : "Message";
}

function drawBody(): void {
  const mode = state.mode;
  if (mode.kind === "new-message") {
    show(
      body,
      pickerView(mode.to, "To", "to"),
      h("div", { class: "empty" }, mode.to.picked.length ? "Write your message below." : "Who is it for?"),
    );
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
    const list = h(
      "div",
      { class: "messages", "data-test": "messages" },
      ...(state.messages.length ? state.messages.map((m, i) => messageView(m, state.messages[i - 1])) : [h("div", { class: "empty" }, "No messages yet. Say hello.")]),
      state.sending
        ? h(
            "div",
            { class: "message pending", "data-test": "sending" },
            h("span", { class: "avatar" }, "…"),
            h("div", { class: "message-body" }, h("div", { class: "message-head" }, h("span", { class: "sender" }, state.sending.waiting ?? "Sending…")), h("div", { class: "message-text" }, state.sending.text)),
          )
        : null,
    );
    show(body, list);
    list.scrollTop = list.scrollHeight;
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

function renderHead(): void {
  const mode = state.mode;
  const c = mode.kind === "conversation" ? state.conversations.find((x) => x.id === mode.id) : undefined;
  const title = mode.kind === "new-message" ? "New message" : mode.kind === "new-channel" ? "New channel" : (c?.title ?? "Chat");
  show(
    head,
    h("h1", {}, title),
    c && c.kind !== "direct" ? h("span", { class: "muted people" }, c.people.map((p) => (p.login === state.me ? "You" : p.name)).join(", ")) : null,
  );
}

function render(): void {
  renderSidebar();
  renderHead();
  show(
    banner,
    state.notice && h("div", { class: `banner ${state.notice.tone === "ok" ? "done" : "refused"}`, role: "status" }, state.notice.text),
    state.offline && h("div", { class: "banner offline" }, "Can't reach the system right now. Nothing you write is sent until it's back."),
  );
  renderBody();
  show(hint, bridge.asks("chat.post") ? "Your organization asks you to approve each message on the desktop." : "Enter to send · Shift+Enter for a new line");
}

async function start(): Promise<void> {
  show(root, sidebar, h("main", { class: "pane" }, head, banner, body, h("div", { class: "composer-wrap" }, composer, hint)));
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
