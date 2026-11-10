// Mail (UI-5, NV-3, ADR 0037): mailboxes, a reading pane, reply and
// compose. Every read and send is an action through the bridge; sends that
// ask wait for the user on the desktop, and the app says so while they do.
// What an agent wrote carries "by [agent] for [user]" inline (AU-10).
//
// With IT's mail service, the person connects once with their password
// (mail.connect, theirs alone: glassd keeps it, the page never does), and
// files mail in Archive, Junk or Trash (mail.move). A message's HTML is
// shown in a frame on its own origin with no scripts and no network, so no
// remote image tells the sender it was read; everything else is text. New
// mail arrives through app.status waits, ids only. Opening a message marks
// it read, here and on the server (mail.mark, ADR 0050); an agent reading
// it never does.

import {
  addressField,
  type AppStatus,
  attributionChip,
  Bridge,
  BridgeError,
  firstCursor,
  h,
  keepFilesOffline,
  Offline,
  type Person,
  ready,
  recentPeople,
  redraw,
  showAttribution,
  watch,
} from "@glasshouse/sdk";
import {
  type Destination,
  FOLDERS,
  type Folder,
  type Message,
  type Outgoing,
  type Summary,
  displayName,
  draftFor,
  fileSize,
  fullDate,
  initials,
  listDate,
  moves,
  parseRecipients,
  problems,
  replyTo,
  tracksReading,
  viewUrl,
  wasSent,
} from "./mail.ts";

/** Connecting mail: the form, until it's done. The password lives here only while typed. */
interface Connect {
  user: string;
  password: string;
  error: string | null;
  busy: boolean;
}

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
  /** The To field, made once per message so its suggestions survive redraws. */
  toField: HTMLElement | null;
  subject: string;
  body: string;
  /** The draft it came from: sending it uses this text. */
  draft: string | null;
  errors: string[];
}

const bridge = new Bridge({ app: "mail" });
const state = {
  folder: "inbox" as Folder,
  lists: { inbox: [], drafts: [], sent: [], archive: [], junk: [], trash: [] } as Record<Folder, Summary[]>,
  /** How many in each folder the person hasn't opened, listed or not. */
  unread: {} as Partial<Record<Folder, number>>,
  selected: null as string | null,
  message: null as Message | null,
  compose: null as Compose | null,
  connect: null as Connect | null,
  outbox: [] as Pending[],
  offline: false,
  loadedAt: null as number | null,
  notice: null as string | null,
  user: "",
  account: null as Account | null,
  /** Where app.status waits pick up (ids only). */
  cursor: 0,
};

/** Who mail goes as and through which service (`mail.account`, NV-3). */
interface Account {
  name: string;
  address: string | null;
  service_title?: string | null;
  tenant?: string | null;
  connected: boolean;
  /** IT's service is one this computer connects to with the person's password. */
  connectable?: boolean;
}
let nextKey = 1;

const root = document.getElementById("app") as HTMLElement;

async function loadFolder(folder: Folder): Promise<void> {
  const read = await bridge.read<{ messages: Summary[]; unread?: number }>("mail.list", { folder, limit: 100 });
  if (read.result.ok) {
    state.lists[folder] = read.result.data.messages;
    state.unread[folder] = read.result.data.unread ?? 0;
  }
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
  state.notice = null;
  render();
  try {
    const read = await bridge.read<{ message: Message }>("mail.read", { id });
    noteOffline(read.offline, read.loadedAt);
    state.message = read.result.ok ? read.result.data.message : null;
    if (!read.result.ok) state.notice = read.result.text;
    // The person opened it: read now (an agent's mail.read never marks).
    if (state.message?.unread && !read.offline) void mark(state.message, true);
  } catch (error) {
    state.message = null;
    state.notice = error instanceof Offline ? "This message wasn't loaded before going offline." : String(error);
  }
  render();
}

/** The folders on screen: the Inbox, Drafts and Sent (replies and sends read them), and the one open. */
async function refresh(): Promise<void> {
  const folders = new Set<Folder>(["inbox", "drafts", "sent", state.folder]);
  try {
    await Promise.all([...folders].map((f) => loadFolder(f)));
  } catch (error) {
    state.notice = error instanceof Offline ? "Mail hasn't been loaded on this device yet." : String(error);
  }
  render();
}

/** Marks a message read or unread (mail.mark); undo on the desktop marks it back. */
async function mark(message: Summary, read: boolean): Promise<void> {
  try {
    const result = await bridge.call("mail.mark", { ids: [message.id], read });
    if (!result.ok) {
      state.notice = result.text;
      return render();
    }
    for (const m of [...state.lists[message.folder], state.message]) {
      if (m?.id === message.id && m.unread === read) {
        m.unread = !read;
        if (m !== state.message) state.unread[message.folder] = Math.max(0, (state.unread[message.folder] ?? 0) + (read ? -1 : 1));
      }
    }
    if (!read) {
      // Back to the list, so it stays unread until it's opened again.
      state.selected = null;
      state.message = null;
    }
  } catch (error) {
    state.notice = error instanceof Offline ? "Marking needs the system; it isn't answering." : String(error);
  }
  render();
}

/** Files the message on screen (mail.move); undo on the desktop moves it back. */
async function moveTo(message: Message, to: Destination, label: string): Promise<void> {
  try {
    const result = await bridge.call("mail.move", { ids: [message.id], to });
    if (!result.ok) {
      state.notice = result.text;
      return render();
    }
    state.notice = label === "Not junk" ? "Moved to Inbox, and marked as not junk." : result.text;
    state.selected = null;
    state.message = null;
    await Promise.all([loadFolder(message.folder), loadFolder(to)]);
  } catch (error) {
    state.notice = error instanceof Offline ? "Moving needs the system; it isn't answering." : String(error);
  }
  render();
}

function startConnect(): void {
  state.compose = null;
  state.notice = null;
  state.connect = { user: state.account?.address ?? "", password: "", error: null, busy: false };
  render();
  root.querySelector<HTMLInputElement>("input[name=password]")?.focus();
}

/** mail.connect: the person's own, with their mail password, which only glassd keeps. */
async function connect(form: Connect): Promise<void> {
  form.busy = true;
  form.error = null;
  render();
  try {
    const input: { password: string; user?: string } = { password: form.password };
    if (form.user.trim()) input.user = form.user.trim();
    const result = await bridge.call("mail.connect", input);
    form.password = "";
    if (!result.ok) {
      form.error = result.text;
      form.busy = false;
      return render();
    }
    state.connect = null;
    state.notice = result.text;
    await Promise.all([loadAccount(), refresh()]);
  } catch (error) {
    form.password = "";
    form.busy = false;
    form.error = error instanceof Offline ? "Connecting needs the system; it isn't answering." : String(error);
  }
  render();
}

/** New mail, from app.status waits (ids only): the folders on screen reload. */
function live(): void {
  // The system going away under an open page is said when it happens, and
  // taken back when it answers again: both by reading the folders again,
  // which is where "offline" and "loaded at" come from. It used to show at
  // the person's next click, so a page left open looked up to date.
  watch(bridge, "mail", state.cursor, refresh, { onOffline: () => void refresh() });
}

function startCompose(from?: Outgoing, draft: string | null = null): void {
  state.notice = null;
  state.compose = {
    to: from?.to.join(", ") ?? "",
    toField: null,
    subject: from?.subject ?? "",
    body: from?.body ?? "",
    draft,
    errors: [],
  };
  render();
  root.querySelector<HTMLInputElement>(from ? "textarea" : "input[name=to]")?.focus();
}

/** Colleagues in the org's directory whose name or address starts with `query` (AD-1). */
async function lookup(query: string): Promise<Person[]> {
  const found = await bridge.call<{ people: Person[] }>("people.search", { query, limit: 8 });
  // Someone with no address can't be mailed or invited.
  return found.ok ? found.data.people.filter((p) => p.email) : [];
}

/** People from the mail on this computer: who wrote and who was written to. */
function recent(): Person[] {
  const lists = Object.values(state.lists).flat();
  return recentPeople(lists.flatMap((m) => [m.from, ...m.to]));
}

function outgoing(compose: Compose): { message: Outgoing; errors: string[] } {
  const { to, bad } = parseRecipients(compose.to);
  // Editing a draft names it: sending or saving it takes its place, so it
  // isn't left in Drafts to go again (DP-30).
  const message: Outgoing = { to, subject: compose.subject.trim(), body: compose.body, ...(compose.draft ? { draft: compose.draft } : {}) };
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

/**
 * Draws the page from the state, changing what is on screen to match
 * instead of replacing it. New mail can arrive while someone writes a
 * message or reads down a long list: replaced whole, the page took the
 * keyboard from them mid-sentence, sent the list back to its top, and
 * loaded an HTML message's frame again (each load an audited mail.read).
 */
function render(): void {
  redraw(root, () => [titlebar(), h("div", { class: "body" }, sidebar(), list(), pane())]);
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
  // Unread mail where folders keep read state; how many drafts wait.
  const count = (folder: Folder) => (folder === "drafts" ? state.lists.drafts.length : tracksReading(folder) ? (state.unread[folder] ?? 0) : 0);
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
            state.notice = null;
            state.selected = null;
            state.message = null;
            state.compose = null;
            state.connect = null;
            render();
            void loadFolder(f.id)
              .catch(() => undefined)
              .then(render);
          },
          "data-folder": f.id,
        },
        h("span", {}, f.title),
        h("span", { class: "count" }, count(f.id) || ""),
      ),
    ),
    state.account
      ? h(
          "div",
          { class: "account", "data-test": "account", title: state.account.connected ? "" : "Sent mail stays in Sent on this computer until the mail service is connected." },
          h("div", { class: "account-address" }, state.account.address ?? state.account.name),
          h(
            "div",
            { class: "account-service" },
            state.account.service_title
              ? `${state.account.service_title}${state.account.tenant ? ` · ${state.account.tenant}` : ""}${state.account.connected ? "" : " · not connected yet"}`
              : "No mail service from IT yet",
          ),
          state.account.connectable && !state.account.connected
            ? h(
                "button",
                { class: "button primary connect", disabled: state.offline, onclick: () => startConnect(), "data-test": "connect" },
                "Connect mail",
              )
            : null,
        )
      : null,
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
          class: `row${state.selected === m.id ? " selected" : ""}${m.unread ? " unread" : ""}`,
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
  // Shown until the person goes somewhere else (`open`, a folder, a new
  // message). It was cleared as it was drawn, so whatever drew the page
  // next (the account loading, mail arriving) took it away: at a first
  // start with the system not answering, the reason never showed at all.
  const notice = state.notice ? h("div", { class: "notice", "data-test": "notice" }, state.notice) : null;
  return h(
    "main",
    { class: "pane" },
    banner,
    refused,
    notice,
    state.connect
      ? connector(state.connect)
      : state.compose
        ? composer(state.compose)
        : state.message
          ? reading(state.message)
          : h("div", { class: "empty" }, "Select a message"),
  );
}

/** Connecting mail with the person's password (mail.connect). */
function connector(form: Connect): HTMLElement {
  const service = state.account?.service_title ?? "your mail service";
  const user = h("input", {
    name: "user",
    value: form.user,
    autocomplete: "username",
    oninput: (e: Event) => {
      form.user = (e.target as HTMLInputElement).value;
    },
  });
  const password = h("input", {
    name: "password",
    type: "password",
    autocomplete: "current-password",
    oninput: (e: Event) => {
      form.password = (e.target as HTMLInputElement).value;
    },
  });
  password.value = form.password;
  return h(
    "form",
    {
      class: "composer connect-form",
      "data-test": "connect-form",
      onsubmit: (e: Event) => {
        e.preventDefault();
        if (!form.password || form.busy) return;
        void connect(form);
      },
    },
    h("h2", {}, "Connect mail"),
    h(
      "p",
      { class: "muted" },
      `Your password for ${service}, once. It stays with this computer's system, never in Mail, and no agent can use it to connect.`,
    ),
    h("label", { class: "field" }, h("span", {}, "Sign in as"), user),
    h("label", { class: "field" }, h("span", {}, "Password"), password),
    form.error ? h("div", { class: "banner refused", "data-test": "connect-error" }, form.error) : null,
    h(
      "div",
      { class: "actions" },
      h("button", { class: "button primary", type: "submit", disabled: form.busy, "data-test": "connect-submit" }, form.busy ? "Connecting…" : "Connect"),
      h(
        "button",
        {
          class: "button",
          type: "button",
          onclick: () => {
            state.connect = null;
            render();
          },
        },
        "Cancel",
      ),
    ),
  );
}

/** The message itself: its HTML framed with no scripts and no network, else its text. */
function body(message: Message): HTMLElement {
  const url = message.html ? viewUrl(location, message.id) : null;
  if (!url) return h("div", { class: "text" }, message.body);
  return h("iframe", {
    class: "html",
    src: url,
    sandbox: true,
    referrerpolicy: "no-referrer",
    title: `${message.subject || "The message"}, as sent`,
    "data-test": "html",
  });
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
    body(message),
    message.attachments?.length
      ? h(
          "div",
          { class: "attachments", "data-test": "attachments" },
          ...message.attachments.map((a) =>
            h("span", { class: "attachment", title: a.type }, h("span", { class: "clip", "aria-hidden": "true" }, "📎"), a.name, h("span", { class: "muted" }, ` · ${fileSize(a.size)}`)),
          ),
        )
      : null,
    h(
      "div",
      { class: "actions" },
      ...moves(message.folder).map((m) =>
        h(
          "button",
          { class: "button", disabled: state.offline, onclick: () => void moveTo(message, m.to, m.label), "data-test": `move-${m.to}` },
          m.label,
        ),
      ),
      tracksReading(message.folder)
        ? h("button", { class: "button", disabled: state.offline, onclick: () => void mark(message, false), "data-test": "mark-unread" }, "Mark unread")
        : null,
      isDraft
        ? h(
            "button",
            {
              class: "button primary",
              disabled: state.offline || sent || waiting,
              onclick: () => void send({ to: message.to, subject: message.subject, body: message.body, draft: message.id }),
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
    h("div", { class: "draft-snippet" }, draft.snippet.length >= 140 ? `${draft.snippet}…` : draft.snippet),
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
  compose.toField ??= addressField({
    name: "to",
    label: "To",
    value: compose.to,
    lookup,
    recent,
    onchange: (value) => {
      compose.to = value;
    },
  });
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
    h("div", { class: "field from", "data-test": "from" }, h("span", {}, "From"), h("span", { class: "from-address" }, sender())),
    compose.toField,
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

/** The account IT set up (NV-3); offline, Mail goes on without it. */
async function loadAccount(): Promise<void> {
  try {
    const read = await bridge.read<Account>("mail.account", {});
    if (read.result.ok) state.account = read.result.data;
  } catch {
    // Not loaded before going offline: nothing to show.
  }
}

/** "Priya Shah <priya@acme.example>", or the login without an address. */
function sender(): string {
  const a = state.account;
  if (!a) return state.user;
  return a.address ? `${a.name} <${a.address}>` : a.name;
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
  // The cursor before the first load, so mail that comes in between wakes
  // the page (NV-3, ADR 0051).
  // Offline, 0: what was kept is shown, and the first wait tries again.
  state.cursor = await firstCursor(bridge);
  await Promise.all([refresh(), loadAccount()]);
  // The account may answer after the mail is drawn: draw it too.
  render();
  ready();
  // From here, new mail wakes the page (ADR 0037), once the system answers
  // if it doesn't yet.
  live();
}

void start();
