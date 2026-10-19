// Mail (UI-5, NV-3): mailboxes, a reading pane, reply and compose. Every
// read and send is an action through the bridge; sends that ask wait for
// the user on the desktop, and the app says so while they do. What an
// agent wrote carries "by [agent] for [user]" inline (AU-10).

import {
  type AppStatus,
  Bridge,
  BridgeError,
  Offline,
  attributionChip,
  h,
  keepFilesOffline,
  ready,
  show,
  showAttribution,
} from "@glasshouse/sdk";
import {
  FOLDERS,
  type Folder,
  type Message,
  type Outgoing,
  type Summary,
  displayName,
  draftFor,
  fullDate,
  initials,
  listDate,
  parseRecipients,
  problems,
  replyTo,
  wasSent,
} from "./mail.ts";

/** A send this window started, until it's done. */
interface Pending {
  key: number;
  message: Outgoing;
  state: "waiting" | "sending" | "sent" | "declined" | "refused" | "offline";
  /** What the desktop shows the user, once it asks. */
  asked: string | null;
  text: string;
}

interface Compose {
  to: string;
  subject: string;
  body: string;
  /** The draft it came from: sending it uses this text. */
  draft: string | null;
  errors: string[];
}

const bridge = new Bridge({ app: "mail" });
const state = {
  folder: "inbox" as Folder,
  lists: { inbox: [], drafts: [], sent: [] } as Record<Folder, Summary[]>,
  selected: null as string | null,
  message: null as Message | null,
  compose: null as Compose | null,
  outbox: [] as Pending[],
  offline: false,
  loadedAt: null as number | null,
  notice: null as string | null,
  user: "",
};
let nextKey = 1;

const root = document.getElementById("app") as HTMLElement;

async function loadFolder(folder: Folder): Promise<void> {
  const read = await bridge.read<{ messages: Summary[] }>("mail.list", { folder, limit: 100 });
  if (read.result.ok) state.lists[folder] = read.result.data.messages;
  noteOffline(read.offline, read.loadedAt);
}

function noteOffline(offline: boolean, loadedAt?: number): void {
  state.offline = offline;
  if (offline && loadedAt) state.loadedAt = Math.min(state.loadedAt ?? loadedAt, loadedAt);
  if (!offline) state.loadedAt = null;
}

async function open(id: string): Promise<void> {
  state.selected = id;
  state.compose = null;
  render();
  try {
    const read = await bridge.read<{ message: Message }>("mail.read", { id });
    noteOffline(read.offline, read.loadedAt);
    state.message = read.result.ok ? read.result.data.message : null;
    if (!read.result.ok) state.notice = read.result.text;
  } catch (error) {
    state.message = null;
    state.notice = error instanceof Offline ? "This message wasn't loaded before going offline." : String(error);
  }
  render();
}

async function refresh(): Promise<void> {
  try {
    await Promise.all(FOLDERS.map((f) => loadFolder(f.id)));
  } catch (error) {
    state.notice = error instanceof Offline ? "Mail hasn't been loaded on this device yet." : String(error);
  }
  render();
}

function startCompose(from?: Outgoing, draft: string | null = null): void {
  state.compose = {
    to: from?.to.join(", ") ?? "",
    subject: from?.subject ?? "",
    body: from?.body ?? "",
    draft,
    errors: [],
  };
  render();
  root.querySelector<HTMLInputElement>(from ? "textarea" : "input[name=to]")?.focus();
}

function outgoing(compose: Compose): { message: Outgoing; errors: string[] } {
  const { to, bad } = parseRecipients(compose.to);
  const message = { to, subject: compose.subject.trim(), body: compose.body };
  return { message, errors: problems(message, bad) };
}

async function saveDraft(): Promise<void> {
  const compose = state.compose;
  if (!compose) return;
  const { message, errors } = outgoing(compose);
  compose.errors = errors;
  if (errors.length) return render();
  try {
    const result = await bridge.call("mail.draft", message);
    if (!result.ok) {
      compose.errors = [result.text];
      return render();
    }
    state.compose = null;
    state.notice = "Saved to Drafts.";
    await refresh();
  } catch (error) {
    compose.errors = [error instanceof Offline ? "Saving needs the system; it isn't answering." : String(error)];
    render();
  }
}

async function send(message: Outgoing): Promise<void> {
  const pending: Pending = {
    key: nextKey++,
    message,
    state: bridge.asks("mail.send") ? "waiting" : "sending",
    asked: null,
    text: "",
  };
  state.outbox.push(pending);
  state.compose = null;
  render();
  // While the desktop asks, show what it asks (app.status).
  const watch = setInterval(() => {
    bridge
      .status()
      .then((status: AppStatus) => {
        const asked = status.waiting?.summary ?? null;
        if (asked !== pending.asked) {
          pending.asked = asked;
          if (asked) pending.state = "waiting";
          render();
        }
      })
      .catch(() => undefined);
  }, 400);
  try {
    const result = await bridge.call("mail.send", { messages: [message] });
    pending.text = result.text;
    pending.state = result.ok ? "sent" : result.text.startsWith("Declined") ? "declined" : "refused";
  } catch (error) {
    pending.state = "offline";
    pending.text =
      error instanceof Offline
        ? "Not sent: the system isn't answering. Nothing left this device."
        : error instanceof BridgeError
          ? error.message
          : String(error);
  } finally {
    clearInterval(watch);
  }
  render();
  await refresh();
}

// ---- rendering ------------------------------------------------------------

function render(): void {
  show(root, titlebar(), h("div", { class: "body" }, sidebar(), list(), pane()));
}

function titlebar(): HTMLElement {
  const locked = state.offline;
  return h(
    "header",
    { class: "titlebar" },
    h("h1", {}, "Mail"),
    h("span", { class: "muted" }, FOLDERS.find((f) => f.id === state.folder)?.title ?? ""),
    h("span", { class: "spacer" }),
    state.user ? h("span", { class: "user" }, state.user) : null,
    h(
      "button",
      { class: "button primary", disabled: locked, onclick: () => startCompose(), "data-test": "compose" },
      "New message",
    ),
  );
}

function sidebar(): HTMLElement {
  const unread = (folder: Folder) => state.lists[folder].length;
  const waiting = state.outbox.filter((p) => p.state === "waiting" || p.state === "sending");
  const done = state.outbox.filter((p) => p.state !== "waiting" && p.state !== "sending").slice(-3);
  return h(
    "nav",
    { class: "sidebar" },
    ...FOLDERS.map((f) =>
      h(
        "button",
        {
          class: `folder${state.folder === f.id ? " active" : ""}`,
          onclick: () => {
            state.folder = f.id;
            state.selected = null;
            state.message = null;
            state.compose = null;
            render();
          },
          "data-folder": f.id,
        },
        h("span", {}, f.title),
        h("span", { class: "count" }, unread(f.id) || ""),
      ),
    ),
    waiting.length || done.length
      ? h(
          "section",
          { class: "outbox", "data-test": "outbox" },
          h("h2", {}, "Outbox"),
          ...waiting.map((p) =>
            h(
              "div",
              { class: "outbox-row waiting", "data-test": "waiting" },
              h("span", { class: "waiting-dot" }),
              h(
                "div",
                {},
                h("div", { class: "outbox-subject" }, p.message.subject),
                h(
                  "div",
                  { class: "outbox-note" },
                  p.state === "waiting" ? "Waiting for your approval on the desktop" : "Sending…",
                ),
              ),
            ),
          ),
          ...done.map((p) =>
            h(
              "div",
              { class: `outbox-row ${p.state}`, "data-test": `outbox-${p.state}` },
              h("div", {}, h("div", { class: "outbox-subject" }, p.message.subject), h("div", { class: "outbox-note" }, outcome(p))),
            ),
          ),
        )
      : null,
  );
}

function outcome(p: Pending): string {
  switch (p.state) {
    case "sent":
      return "Sent";
    case "declined":
      return "Declined on the desktop; not sent";
    default:
      return p.text;
  }
}

function list(): HTMLElement {
  const messages = state.lists[state.folder];
  const drafts = state.lists.drafts;
  return h(
    "section",
    { class: "list", "aria-label": "Messages" },
    state.offline ? offlineBanner() : null,
    messages.length === 0
      ? h("div", { class: "empty" }, state.folder === "inbox" ? "No mail" : "Nothing here")
      : null,
    ...messages.map((m) => {
      const reply = state.folder === "inbox" ? draftFor(m, drafts) : null;
      const who = state.folder === "inbox" ? displayName(m.from) : m.to.map(displayName).join(", ");
      return h(
        "button",
        {
          class: `row${state.selected === m.id ? " selected" : ""}`,
          onclick: () => void open(m.id),
          "data-id": m.id,
        },
        h("div", { class: "row-top" }, h("span", { class: "row-who" }, who), h("span", { class: "row-date" }, listDate(m.date))),
        h("div", { class: "row-subject" }, m.subject || "(no subject)"),
        h("div", { class: "row-snippet" }, m.snippet),
        showAttribution(m.attribution, "mail") && m.attribution
          ? h("div", { class: "row-chips" }, attributionChip(m.attribution.text))
          : null,
        reply && reply.attribution && showAttribution(reply.attribution, "mail")
          ? h("div", { class: "row-chips" }, h("span", { class: "draft-chip" }, "Reply drafted"), attributionChip(reply.attribution.text))
          : null,
      );
    }),
  );
}

function offlineBanner(): HTMLElement {
  const when = state.loadedAt ? new Date(state.loadedAt).toLocaleTimeString("en-US", { hour: "numeric", minute: "2-digit" }) : null;
  return h(
    "div",
    { class: "banner offline", "data-test": "offline" },
    `Offline · showing what was loaded${when ? ` at ${when}` : ""}. Sending comes back when the system does.`,
  );
}

function pane(): HTMLElement {
  const waiting = state.outbox.find((p) => p.state === "waiting");
  const banner = waiting
    ? h(
        "div",
        { class: "banner waiting", "data-test": "waiting-banner" },
        h("span", { class: "waiting-dot" }),
        h(
          "span",
          {},
          h("strong", {}, "Waiting for your approval on the desktop. "),
          waiting.asked ?? `Send email to ${waiting.message.to.join(", ")}: ${waiting.message.subject}`,
        ),
      )
    : null;
  const last = state.outbox.at(-1);
  const refused =
    last && (last.state === "declined" || last.state === "refused" || last.state === "offline")
      ? h("div", { class: "banner refused", "data-test": "refused" }, outcome(last))
      : null;
  const notice = state.notice ? h("div", { class: "notice" }, state.notice) : null;
  state.notice = null;
  return h(
    "main",
    { class: "pane" },
    banner,
    refused,
    notice,
    state.compose ? composer(state.compose) : state.message ? reading(state.message) : h("div", { class: "empty" }, "Select a message"),
  );
}

function reading(message: Message): HTMLElement {
  const draft = message.folder === "inbox" ? draftFor(message, state.lists.drafts) : null;
  const isDraft = message.folder === "drafts";
  const sent = isDraft && wasSent(message, state.lists.sent);
  const waiting = state.outbox.some(
    (p) =>
      (p.state === "waiting" || p.state === "sending") &&
      p.message.subject === message.subject &&
      p.message.to.join() === message.to.join(),
  );
  return h(
    "article",
    { class: "message", "data-test": "message" },
    h("h2", { class: "subject" }, message.subject || "(no subject)"),
    h(
      "div",
      { class: "meta" },
      h("span", { class: "avatar" }, initials(message.from)),
      h(
        "div",
        {},
        h("div", { class: "from" }, displayName(message.from), h("span", { class: "muted" }, ` to ${message.to.join(", ")}`)),
        h("div", { class: "muted" }, fullDate(message.date)),
      ),
      h("span", { class: "spacer" }),
      showAttribution(message.attribution, "mail") && message.attribution ? attributionChip(message.attribution.text) : null,
    ),
    h("div", { class: "text" }, message.body),
    h(
      "div",
      { class: "actions" },
      isDraft
        ? h(
            "button",
            {
              class: "button primary",
              disabled: state.offline || sent || waiting,
              onclick: () => void send({ to: message.to, subject: message.subject, body: message.body }),
              "data-test": "send-draft",
            },
            sent ? "Sent" : waiting ? "Waiting for approval" : "Send",
          )
        : null,
      isDraft
        ? h("button", { class: "button", disabled: state.offline, onclick: () => startCompose({ to: message.to, subject: message.subject, body: message.body }, message.id) }, "Edit")
        : h("button", { class: "button", disabled: state.offline, onclick: () => startCompose(replyTo(message)), "data-test": "reply" }, "Reply"),
    ),
    draft ? draftCard(draft) : null,
  );
}

/** The reply an agent drafted to the message on screen (AU-10). */
function draftCard(draft: Summary): HTMLElement {
  const sent = wasSent(draft, state.lists.sent);
  return h(
    "section",
    { class: "draft-card", "data-test": "draft-card" },
    h(
      "div",
      { class: "draft-head" },
      h("strong", {}, sent ? "Your reply was sent" : "Reply drafted"),
      draft.attribution && showAttribution(draft.attribution, "mail") ? attributionChip(draft.attribution.text) : null,
    ),
    h("div", { class: "draft-snippet" }, draft.snippet),
    h(
      "div",
      { class: "actions" },
      h(
        "button",
        { class: "button primary", disabled: state.offline || sent, onclick: () => void open(draft.id), "data-test": "review-draft" },
        "Review and send",
      ),
    ),
  );
}

function composer(compose: Compose): HTMLElement {
  const field = (name: "to" | "subject", label: string) =>
    h(
      "label",
      { class: "field" },
      h("span", {}, label),
      h("input", {
        name,
        value: compose[name],
        autocomplete: "off",
        oninput: (e: Event) => {
          compose[name] = (e.target as HTMLInputElement).value;
        },
      }),
    );
  const body = h("textarea", {
    name: "body",
    rows: 14,
    oninput: (e: Event) => {
      compose.body = (e.target as HTMLTextAreaElement).value;
    },
  });
  body.value = compose.body;
  const asks = bridge.asks("mail.send");
  return h(
    "form",
    {
      class: "composer",
      "data-test": "composer",
      onsubmit: (e: Event) => {
        e.preventDefault();
        const { message, errors } = outgoing(compose);
        compose.errors = errors;
        if (errors.length) return render();
        void send(message);
      },
    },
    h("h2", {}, compose.draft ? "Edit draft" : compose.subject.startsWith("Re:") ? "Reply" : "New message"),
    field("to", "To"),
    field("subject", "Subject"),
    body,
    ...compose.errors.map((e) => h("div", { class: "banner refused" }, e)),
    h(
      "div",
      { class: "actions" },
      h("button", { class: "button primary", type: "submit", "data-test": "send" }, "Send"),
      h("button", { class: "button", type: "button", onclick: () => void saveDraft() }, "Save draft"),
      h("button", { class: "button", type: "button", onclick: () => { state.compose = null; render(); } }, "Discard"),
      asks ? h("span", { class: "hint" }, "Sending waits for your approval on the desktop.") : null,
    ),
  );
}

// ---- start ----------------------------------------------------------------

async function start(): Promise<void> {
  keepFilesOffline();
  try {
    const session = await bridge.open();
    state.user = session.user;
  } catch (error) {
    state.notice = error instanceof Offline ? "Mail hasn't been loaded on this device yet." : String(error);
  }
  await refresh();
  ready();
}

void start();
