// Settings (UX-1, ADR 0029): how this computer is set up for the person.
// IT sets some for their groups, as defaults they can change or locked;
// the rest are theirs. Locked ones show as set by IT and can't be changed
// here; the browser's are the whole computer's, so IT sets them. Every
// change goes through the bridge and the audit log like any action.

import { Bridge, BridgeError, Offline, h, ready, show } from "@glasshouse/sdk";

type Value = boolean | number | string | string[];

interface Setting {
  id: string;
  section: string;
  title: string;
  help: string;
  kind: { type: "toggle" } | { type: "number"; min: number; max: number; unit: string } | { type: "url" } | { type: "sites" };
  scope: "person" | "device";
  default: Value;
  value: Value;
  locked: boolean;
  /** Who decided it: IT, the person, or nobody (the computer's default). */
  from: "it" | "you" | "default";
}

const bridge = new Bridge({ app: "settings" });
const state = {
  settings: [] as Setting[],
  notice: null as { text: string; tone: "ok" | "bad" } | null,
  offline: false,
  busy: null as string | null,
};
const root = document.getElementById("app") as HTMLElement;

async function load(): Promise<void> {
  try {
    const read = await bridge.read<{ settings: Setting[] }>("settings.list", {});
    state.offline = read.offline;
    if (read.result.ok) state.settings = read.result.data.settings;
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

/** A value in words, for what the person can't change here. */
function words(setting: Setting): string {
  const v = setting.value;
  if (typeof v === "boolean") return v ? "On" : "Off";
  if (typeof v === "number") return setting.kind.type === "number" ? `${v} ${setting.kind.unit}` : String(v);
  if (Array.isArray(v)) return v.length ? v.join(", ") : "None";
  return v || "None";
}

function who(setting: Setting): HTMLElement | null {
  if (setting.locked) return h("div", { class: "who it", "data-test": "locked" }, "🔒 Set by your organization");
  if (setting.scope === "device") {
    return h("div", { class: "who" }, setting.from === "it" ? "Your organization's default for this computer" : "This computer's default");
  }
  if (setting.from === "it") return h("div", { class: "who" }, "Your organization's default");
  return null;
}

function control(setting: Setting): HTMLElement {
  const busy = state.busy === setting.id || state.offline;
  // The whole computer's, and what IT locked, show their value only.
  if (setting.locked || setting.scope === "device") return h("span", { class: "muted" }, words(setting));
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
}

void load();
