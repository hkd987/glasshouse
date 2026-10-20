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
  kind: { type: "toggle" } | { type: "number"; min: number; max: number; unit: string } | { type: "url" } | { type: "sites" } | { type: "time_zone" } | { type: "apps" };
  scope: "person" | "device" | "seat";
  default: Value;
  value: Value;
  locked: boolean;
  /** Who decided it: IT, the person, the computer's owner, or nobody (the computer's default). */
  from: "it" | "you" | "owner" | "default";
  /** The person may change it here: glassd decides (ADR 0038). */
  editable?: boolean;
}

const bridge = new Bridge({ app: "settings" });
const state = {
  settings: [] as Setting[],
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
  if (setting.kind.type === "sites") {
    const list = Array.isArray(setting.value) ? setting.value : [];
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
          placeholder: "One per line: games.example",
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
    ),
  );
  const page = root.querySelector(".page");
  if (page) page.scrollTop = scrolled;
}

void load();
