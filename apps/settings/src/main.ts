// Settings (UX-1, ADR 0029): how this computer is set up for the person.
// IT sets some for their groups, as defaults they can change or locked;
// the rest are theirs. Locked ones show as set by IT and can't be changed
// here; the computer's (the browser's, USB drives, the time zone) are IT's
// alone, those for the person at the screen from their groups. On a
// computer no organization manages, its owner sets those (ADR 0038).
// glassd says which the person may change. Every change goes through the
// bridge and the audit log like any action.

import { Bridge, BridgeError, Offline, h, ready, show } from "@glasshouse/sdk";

type Value = boolean | number | string | string[];

interface Setting {
  id: string;
  section: string;
  title: string;
  help: string;
  kind: { type: "toggle" } | { type: "number"; min: number; max: number; unit: string } | { type: "url" } | { type: "sites" } | { type: "time_zone" } | { type: "apps" } | { type: "folders" };
  scope: "person" | "device" | "seat";
  default: Value;
  value: Value;
  locked: boolean;
  /** Who decided it: IT, the person, the computer's owner, or nobody (the computer's default). */
  from: "it" | "you" | "owner" | "default";
  /** The person may change it here: glassd decides (ADR 0038). */
  editable?: boolean;
}

/** How agents reach AI models (ADR 0039): the service, never the key. */
interface Models {
  connected: string | null;
  setup_choice: string | null;
  /** IT decides on a managed computer. */
  managed: boolean;
  /** The person is this computer's owner. */
  editable: boolean;
  services: { id: string; name: string }[];
  /** The agents that may ask the owner's models, on this computer they own. */
  agents: { id: string; name: string; on: boolean; always: boolean }[];
}

const bridge = new Bridge({ app: "settings" });
const state = {
  settings: [] as Setting[],
  models: null as Models | null,
  /** What the owner is connecting: the key is typed here and sent once. */
  connecting: { service: "openrouter", key: "" },
  /** The names of the apps settings list, by id, as this computer offers them. */
  appNames: {} as Record<string, string>,
  notice: null as { text: string; tone: "ok" | "bad" } | null,
  offline: false,
  busy: null as string | null,
};
const root = document.getElementById("app") as HTMLElement;

async function load(): Promise<void> {
  try {
    const read = await bridge.read<{ settings: Setting[]; app_names?: Record<string, string> }>("settings.list", {});
    state.offline = read.offline;
    if (read.result.ok) {
      state.settings = read.result.data.settings;
      state.appNames = read.result.data.app_names ?? {};
    }
    else state.notice = { text: read.result.text, tone: "bad" };
  } catch (error) {
    state.notice = { text: error instanceof Offline ? "Settings haven't been loaded on this computer yet." : String(error), tone: "bad" };
  }
  // AI models (ADR 0039) on their own: a system without them still shows
  // every setting, just not this section.
  try {
    const models = await bridge.read<Models>("settings.models", {});
    state.models = models.result.ok ? models.result.data : null;
  } catch {
    state.models = null;
  }
  render();
  ready();
}

async function change(setting: Setting, value: Value | undefined): Promise<void> {
  state.busy = setting.id;
  state.notice = null;
  render();
  try {
    const result = await bridge.call("settings.set", value === undefined ? { id: setting.id } : { id: setting.id, value });
    state.notice = { text: result.text, tone: result.ok ? "ok" : "bad" };
  } catch (error) {
    state.notice = {
      text: error instanceof Offline ? "That needs the system, and it isn't answering. Nothing changed." : error instanceof BridgeError ? error.message : String(error),
      tone: "bad",
    };
  }
  state.busy = null;
  await load();
}

/** An app's name as this computer offers it; its id for one it no longer has. */
function appName(id: string): string {
  return state.appNames[id] ?? id;
}

/** A value in words, for what the person can't change here. */
function words(setting: Setting): string {
  const v = setting.value;
  if (typeof v === "boolean") return v ? "On" : "Off";
  if (typeof v === "number") return setting.kind.type === "number" ? `${v} ${setting.kind.unit}` : String(v);
  if (Array.isArray(v)) return v.length ? (setting.kind.type === "apps" ? v.map(appName) : v).join(", ") : "None";
  return v || "None";
}

function who(setting: Setting): HTMLElement | null {
  if (setting.locked) return h("div", { class: "who it", "data-test": "locked" }, "🔒 Set by your organization");
  if (setting.scope !== "person" && setting.from === "owner") return h("div", { class: "who" }, "Chosen by this computer's owner");
  if (setting.scope !== "person" && setting.editable) {
    return h("div", { class: "who" }, setting.from === "you" ? "Your choice, for everyone on this computer" : "This computer's default; you choose for everyone on it");
  }
  if (setting.scope === "device") {
    return h("div", { class: "who" }, setting.from === "it" ? "Your organization's default for this computer" : "This computer's default");
  }
  if (setting.scope === "seat") {
    return h("div", { class: "who" }, setting.from === "it" ? "Your organization's, for you on this computer" : "This computer's default");
  }
  if (setting.from === "it") return h("div", { class: "who" }, "Your organization's default");
  return null;
}

function control(setting: Setting): HTMLElement {
  const busy = state.busy === setting.id || state.offline;
  // What the person can't change shows its value only: glassd says which
  // (an older glassd doesn't, and then only their own unlocked ones).
  const editable = setting.editable ?? (!setting.locked && setting.scope === "person");
  if (!editable) return h("span", { class: "muted" }, words(setting));
  const reset =
    setting.from === "you"
      ? h("button", { class: "link", disabled: busy, onclick: () => change(setting, undefined) }, "Use default")
      : null;
  if (setting.kind.type === "toggle") {
    return h(
      "span",
      { class: "control" },
      reset,
      h("input", {
        class: "switch",
        type: "checkbox",
        name: setting.id,
        "aria-label": setting.title,
        checked: setting.value === true,
        disabled: busy,
        onchange: (e: Event) => change(setting, (e.target as HTMLInputElement).checked),
      }),
    );
  }
  if (setting.kind.type === "number") {
    const { min, max, unit } = setting.kind;
    return h(
      "span",
      { class: "control" },
      reset,
      h("input", {
        type: "number",
        name: setting.id,
        "aria-label": setting.title,
        min,
        max,
        value: String(setting.value),
        disabled: busy,
        onchange: (e: Event) => change(setting, Number((e.target as HTMLInputElement).value)),
      }),
      h("span", { class: "muted" }, unit),
    );
  }
  if (setting.kind.type === "url" || setting.kind.type === "time_zone") {
    return h(
      "span",
      { class: "control" },
      reset,
      h("input", {
        class: "text",
        type: setting.kind.type === "url" ? "url" : "text",
        name: setting.id,
        "aria-label": setting.title,
        placeholder: setting.kind.type === "url" ? "https://example.com" : "America/New_York",
        value: String(setting.value),
        disabled: busy,
        onchange: (e: Event) => change(setting, (e.target as HTMLInputElement).value.trim()),
      }),
    );
  }
  if (setting.kind.type === "sites" || setting.kind.type === "folders") {
    const list = Array.isArray(setting.value) ? setting.value : [];
    const folders = setting.kind.type === "folders";
    return h(
      "span",
      { class: "control" },
      reset,
      h(
        "textarea",
        {
          class: "text",
          name: setting.id,
          "aria-label": setting.title,
          rows: 2,
          placeholder: folders ? "One per line, in your home: Documents" : "One per line: games.example",
          disabled: busy,
          onchange: (e: Event) =>
            change(
              setting,
              (e.target as HTMLTextAreaElement).value
                .split(/[\n,]+/)
                .map((site) => site.trim())
                .filter(Boolean),
            ),
        },
        list.join("\n"),
      ),
    );
  }
  if (setting.kind.type === "apps") {
    // Apps hidden from Ctrl Space (DK-1): hidden from the search's own
    // rows, shown again here one at a time, IT's default ones too.
    const hidden = Array.isArray(setting.value) ? setting.value : [];
    if (!hidden.length) return h("span", { class: "muted" }, "None");
    return h(
      "span",
      { class: "control hidden-apps" },
      ...hidden.map((id) =>
        h(
          "span",
          { class: "hidden-app", "data-app": id },
          h("span", {}, appName(id)),
          h(
            "button",
            { class: "link", disabled: busy, onclick: () => change(setting, hidden.filter((other) => other !== id)) },
            "Show in search",
          ),
        ),
      ),
    );
  }
  return h("span", { class: "muted" }, words(setting));
}

/** Connects the owner's own AI models, or disconnects them (ADR 0039). */
async function connect(service: string | null): Promise<void> {
  state.busy = "models";
  state.notice = null;
  render();
  try {
    const key = state.connecting.key;
    state.connecting.key = "";
    const result = await bridge.call("settings.models_connect", service ? { service, key } : { service: null });
    state.notice = { text: result.text, tone: result.ok ? "ok" : "bad" };
  } catch (error) {
    state.notice = { text: error instanceof BridgeError ? error.message : String(error), tone: "bad" };
  }
  state.busy = null;
  await load();
}

/** Lets an agent ask the owner's AI models, or stops it (ADR 0039). */
async function allow(agent: string, on: boolean): Promise<void> {
  state.busy = "models";
  state.notice = null;
  render();
  try {
    const result = await bridge.call("settings.models_agents", { agent, on });
    state.notice = { text: result.text, tone: result.ok ? "ok" : "bad" };
  } catch (error) {
    state.notice = { text: error instanceof BridgeError ? error.message : String(error), tone: "bad" };
  }
  state.busy = null;
  await load();
}

/** How agents reach AI models: the owner's to connect, IT's when managed. */
function models(m: Models): HTMLElement {
  const name = (id: string | null) => m.services.find((s) => s.id === id)?.name ?? id ?? "";
  const busy = state.busy === "models" || state.offline;
  let control: HTMLElement;
  let who: string | null = null;
  if (m.managed) {
    control = h("span", { class: "muted" }, "Set by your organization");
  } else if (m.connected) {
    control = h(
      "span",
      { class: "control" },
      h("span", {}, `Your ${name(m.connected)} account`),
      m.editable ? h("button", { class: "link", disabled: busy, "data-test": "disconnect", onclick: () => connect(null) }, "Disconnect") : null,
    );
    if (!m.editable) who = "Connected by this computer's owner";
  } else if (m.editable) {
    control = h(
      "span",
      { class: "control" },
      h(
        "select",
        { name: "service", "aria-label": "Service", disabled: busy, onchange: (e: Event) => (state.connecting.service = (e.target as HTMLSelectElement).value) },
        ...m.services.map((s) => h("option", { value: s.id, selected: state.connecting.service === s.id }, s.name)),
      ),
      h("input", {
        class: "text",
        type: "password",
        name: "key",
        autocomplete: "off",
        "aria-label": "Key",
        placeholder: "Your API key",
        disabled: busy,
        oninput: (e: Event) => (state.connecting.key = (e.target as HTMLInputElement).value),
      }),
      h("button", { class: "button", disabled: busy, "data-test": "connect", onclick: () => connect(state.connecting.service) }, "Connect"),
    );
    if (m.setup_choice === "api-key") who = "You chose to use an API key when you set up this computer.";
  } else {
    control = h("span", { class: "muted" }, "None");
    who = "This computer's owner connects them";
  }
  return h(
    "section",
    { class: "group" },
    h("h2", {}, "AI models"),
    h(
      "div",
      { class: "rows" },
      h(
        "div",
        { class: "setting", "data-setting": "models" },
        h(
          "div",
          {},
          h("div", { class: "title" }, "How agents reach AI models"),
          h("div", { class: "help" }, "Your own OpenRouter or Anthropic account. The key stays on this computer, where only the system can read it, and is never shown again."),
          who ? h("div", { class: "who" }, who) : null,
        ),
        control,
      ),
      // Which agents use them: the owner's choice, once they're connected.
      ...(m.connected && !m.managed
        ? m.agents.map((a) =>
            h(
              "div",
              { class: "setting", "data-setting": `models-agent-${a.id}` },
              h(
                "div",
                {},
                h("div", { class: "title" }, a.name),
                h("div", { class: "help" }, a.always ? "Asks AI models whenever it runs: it's what it does." : "May use your AI models when it works for you."),
              ),
              h(
                "span",
                { class: "control" },
                h("input", {
                  type: "checkbox",
                  "aria-label": `${a.name} uses your AI models`,
                  checked: a.on,
                  disabled: busy || a.always || !m.editable,
                  onchange: (e: Event) => allow(a.id, (e.target as HTMLInputElement).checked),
                }),
              ),
            ),
          )
        : []),
    ),
  );
}

function row(setting: Setting): HTMLElement {
  return h(
    "div",
    { class: "setting", "data-setting": setting.id },
    h("div", {}, h("div", { class: "title" }, setting.title), h("div", { class: "help" }, setting.help), who(setting)),
    control(setting),
  );
}

function render(): void {
  const sections = [...new Set(state.settings.map((s) => s.section))];
  // A change redraws the page: it stays where the person was.
  const scrolled = root.querySelector(".page")?.scrollTop ?? 0;
  show(
    root,
    h("header", { class: "titlebar" }, h("h1", {}, "Settings")),
    h(
      "main",
      { class: "page" },
      state.notice && h("div", { class: `banner ${state.notice.tone === "ok" ? "done" : "refused"}`, role: "status" }, state.notice.text),
      state.offline && h("div", { class: "banner offline" }, "Offline: these are the settings as they were last loaded."),
      ...sections.map((section) =>
        h("section", { class: "group" }, h("h2", {}, section), h("div", { class: "rows" }, ...state.settings.filter((s) => s.section === section).map(row))),
      ),
      state.models ? models(state.models) : null,
    ),
  );
  const page = root.querySelector(".page");
  if (page) page.scrollTop = scrolled;
}

void load();
