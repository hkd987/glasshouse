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
  kind: { type: "toggle" } | { type: "number"; min: number; max: number; unit: string } | { type: "url" } | { type: "sites" } | { type: "time_zone" } | { type: "apps" } | { type: "folders" } | { type: "networks" };
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

/** Wi-Fi as glassd drives it (CF-12, ADR 0059). */
interface Network {
  name: string;
  /** dBm, when nearby. */
  signal?: number;
  security?: "open" | "personal" | "enterprise";
  joined: boolean;
  known: boolean;
  /** Joining it failed, as a wrong password does. */
  failed: boolean;
  organization: boolean;
  /** Why it can't be joined here. */
  blocked?: string;
}

interface Wifi {
  present: boolean;
  state: string;
  joined: string | null;
  address: string | null;
  only_organization: boolean;
  networks: Network[];
}

/** The screen's brightness (ADR 0060): none on a screen not dimmed from here. */
interface Brightness {
  present: boolean;
  percent: number | null;
}

/** A patch or update's state, as glassd names them. */
type Trial = "staging" | "staged" | "probation" | "kept" | "failed" | "rolled_back" | "cancelled" | "undone";

/** This computer (ADR 0057, ADR 0061): its version, OS updates and kernel patches. */
interface Computer {
  version: string | null;
  channel: string;
  /** The person is the owner of a computer nobody manages: they install updates. */
  owner: boolean;
  /** The update being installed. */
  update: { version: string; state: Trial; patches: string[] } | null;
  last_update: { version: string; state: Trial; note: string | null } | null;
  /** From a check. */
  available: { version: string; notes?: string; patches?: string[]; blocked?: string } | null;
  checked: string | null;
  /** The kernel patch being tried. */
  patch: {
    id: string;
    summary: string;
    review: string;
    state: Trial;
    label: string;
    ran_minutes: number;
    probation_minutes: number;
    woke: number;
    probation_resumes: number;
  } | null;
  kept: { id: string; revision: number; summary: string; review: string }[];
}

/** Display & sleep's section, where brightness goes first. */
const DISPLAY = "Display & sleep";
/** This computer's section, where its version and patches go first. */
const COMPUTER = "This computer";

const bridge = new Bridge({ app: "settings" });
const state = {
  settings: [] as Setting[],
  models: null as Models | null,
  wifi: null as Wifi | null,
  brightness: null as Brightness | null,
  computer: null as Computer | null,
  /** The network whose password is being typed, and the password. */
  joining: null as { network: string; password: string } | null,
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
  await Promise.all([loadWifi(), loadBrightness(), loadComputer()]);
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

/** The networks around and the one in use; nothing shown on a system without it. */
async function loadWifi(): Promise<void> {
  try {
    const read = await bridge.read<Wifi>("settings.wifi", {});
    state.wifi = read.result.ok ? read.result.data : null;
  } catch {
    state.wifi = null;
  }
}

async function joinWifi(network: string, password: string | null): Promise<void> {
  state.busy = "wifi";
  state.notice = null;
  render();
  try {
    const result = await bridge.call("settings.wifi_join", password ? { network, password } : { network });
    state.notice = { text: result.text, tone: result.ok ? "ok" : "bad" };
    if (result.ok) state.joining = null;
  } catch (error) {
    state.notice = { text: error instanceof BridgeError ? error.message : String(error), tone: "bad" };
  }
  state.busy = null;
  await load();
}

async function forgetWifi(network: string): Promise<void> {
  state.busy = "wifi";
  state.notice = null;
  render();
  try {
    const result = await bridge.call("settings.wifi_forget", { network });
    state.notice = { text: result.text, tone: result.ok ? "ok" : "bad" };
  } catch (error) {
    state.notice = { text: error instanceof BridgeError ? error.message : String(error), tone: "bad" };
  }
  state.busy = null;
  await load();
}

async function loadBrightness(): Promise<void> {
  try {
    const read = await bridge.read<Brightness>("settings.brightness", {});
    state.brightness = read.result.ok ? read.result.data : null;
  } catch {
    state.brightness = null;
  }
}

/** Sets it as the slider lets go: each change is on the record, so not every step of a drag. */
async function setBrightness(percent: number): Promise<void> {
  state.busy = "brightness";
  state.notice = null;
  try {
    const result = await bridge.call<Brightness>("settings.brightness_set", { percent });
    if (result.ok) state.brightness = result.data;
    else state.notice = { text: result.text, tone: "bad" };
  } catch (error) {
    state.notice = { text: error instanceof BridgeError ? error.message : String(error), tone: "bad" };
  }
  state.busy = null;
  render();
}

function brightness(b: Brightness): HTMLElement {
  const percent = b.percent ?? 100;
  const shown = h("span", { class: "muted", "data-test": "brightness-percent" }, `${percent}%`);
  return h(
    "div",
    { class: "setting", "data-setting": "brightness" },
    h("div", {}, h("div", { class: "title" }, "Brightness"), h("div", { class: "help" }, "How bright the screen is. The brightness keys change it too. It never goes fully dark from here.")),
    h(
      "span",
      { class: "control" },
      h("input", {
        type: "range",
        class: "level",
        name: "brightness",
        "aria-label": "Brightness",
        min: 5,
        max: 100,
        step: 5,
        value: String(percent),
        disabled: state.busy === "brightness" || state.offline,
        oninput: (e: Event) => (shown.textContent = `${(e.target as HTMLInputElement).value}%`),
        onchange: (e: Event) => setBrightness(Number((e.target as HTMLInputElement).value)),
      }),
      shown,
    ),
  );
}

async function loadComputer(): Promise<void> {
  try {
    const read = await bridge.read<Computer>("settings.computer", {});
    // A check's answer stays on the page until the next check.
    const checked = state.computer && { available: state.computer.available, checked: state.computer.checked };
    state.computer = read.result.ok ? { ...read.result.data, ...(checked ?? {}) } : null;
  } catch {
    state.computer = null;
  }
}

/** Checks for an update, installs it, or takes a kernel patch out: each on the record. */
async function computerAction(action: "settings.computer_check" | "settings.update_install" | "settings.patch_remove"): Promise<void> {
  state.busy = "computer";
  state.notice = null;
  render();
  try {
    const result = await bridge.call<Computer>(action, {});
    if (result.ok) state.computer = result.data;
    state.notice = { text: result.text, tone: result.ok ? "ok" : "bad" };
  } catch (error) {
    state.notice = { text: error instanceof BridgeError ? error.message : String(error), tone: "bad" };
  }
  state.busy = null;
  render();
}

/** glassd's reasons start lowercase, for logs; here they're sentences. */
function sentence(text: string): string {
  const t = text.trim();
  return t.charAt(0).toUpperCase() + t.slice(1) + (/[.!?]$/.test(t) ? "" : ".");
}

function hours(minutes: number): string {
  return minutes >= 120 ? `${Math.floor(minutes / 60)} hours` : `${minutes} minutes`;
}

function review(r: string): string {
  return r === "reviewed" ? "Reviewed and signed" : "Nobody here reviewed it";
}

/** The version, its update and its kernel patches: rows at the top of This computer. */
function computer(c: Computer): HTMLElement[] {
  const busy = state.busy === "computer" || state.offline;
  const rows: HTMLElement[] = [];
  // The version, and the update: checked and installed by the owner;
  // on a managed computer IT does.
  let updateHelp: string;
  let updateControl: HTMLElement | null = null;
  if (c.update) {
    updateHelp = c.update.state === "staged" ? `${c.update.version} is installed. Restart to use it: if it doesn't start, the computer goes back by itself.` : `${c.update.version} is being installed.`;
  } else if (c.available) {
    updateHelp = c.available.blocked ? `${c.available.version} is available, but ${c.available.blocked}` : `${c.available.version} is available.${c.available.notes ? ` ${c.available.notes}` : ""}`;
    if (c.owner && !c.available.blocked) updateControl = h("button", { class: "button", disabled: busy, "data-test": "update-install", onclick: () => computerAction("settings.update_install") }, "Install");
  } else {
    updateHelp = c.checked ? sentence(c.checked) : (c.last_update?.state === "rolled_back" ? `${c.last_update.version} didn't start, so the computer went back.` : "Updates come signed, and a restart goes back if one doesn't start.");
  }
  if (!c.update && !updateControl) {
    updateControl = c.owner ? h("button", { class: "link", disabled: busy, "data-test": "update-check", onclick: () => computerAction("settings.computer_check") }, "Check for updates") : h("span", { class: "muted" }, "Your organization or this computer's owner updates it");
  }
  rows.push(
    h(
      "div",
      { class: "setting", "data-setting": "version" },
      h("div", {}, h("div", { class: "title" }, c.version ? `Version ${c.version}` : "Version"), h("div", { class: "help", "data-test": "update-help" }, updateHelp)),
      updateControl,
    ),
  );
  // The kernel patch being tried: anyone at the screen takes it back or out.
  if (c.patch) {
    const p = c.patch;
    const served = p.state === "probation" ? ` · ${hours(p.ran_minutes)} of ${hours(p.probation_minutes)}${p.probation_resumes ? `, ${p.woke} of ${p.probation_resumes} wakes` : ""}` : "";
    rows.push(
      h(
        "div",
        { class: "setting", "data-setting": "patch" },
        h("div", {}, h("div", { class: "title" }, p.summary), h("div", { class: "help", "data-test": "patch-state" }, `${p.label}${served}`), h("div", { class: "who" }, `Kernel patch ${p.id} · ${review(p.review)}`)),
        p.state === "staged" || p.state === "probation"
          ? h("button", { class: "link", disabled: busy, "data-test": "patch-remove", onclick: () => computerAction("settings.patch_remove") }, p.state === "staged" ? "Don't try it" : "Take it out")
          : null,
      ),
    );
  }
  for (const k of c.kept) {
    rows.push(
      h(
        "div",
        { class: "setting", "data-setting": `kept-${k.id}` },
        h("div", {}, h("div", { class: "title" }, k.summary), h("div", { class: "help" }, "Kept: this computer runs on it"), h("div", { class: "who" }, `Kernel patch ${k.id} · ${review(k.review)}`)),
        // The last one kept is the one a take-out undoes.
        !c.patch && k === c.kept[c.kept.length - 1]
          ? h("button", { class: "link", disabled: busy, "data-test": "patch-remove", onclick: () => computerAction("settings.patch_remove") }, "Take it out")
          : null,
      ),
    );
  }
  return rows;
}

/** Signal as four bars: -50 dBm and better is all four. */
function bars(dbm: number | undefined): string {
  if (dbm === undefined) return "";
  const n = dbm >= -50 ? 4 : dbm >= -60 ? 3 : dbm >= -70 ? 2 : 1;
  return "▂▄▆█".slice(0, n).padEnd(4, "·");
}

function network(w: Wifi, n: Network): HTMLElement {
  const busy = state.busy === "wifi" || state.offline;
  const typing = state.joining?.network === n.name;
  const notes = [
    n.joined ? (w.address ? `Connected · ${w.address}` : "Connecting…") : null,
    !n.joined && n.failed ? "Couldn't join: the password may be wrong" : null,
    n.organization ? "Your organization's network" : null,
    !n.joined && n.known && !n.organization ? "Joins by itself" : null,
    n.signal === undefined ? "Not in range" : null,
  ].filter(Boolean);
  let control: HTMLElement | null;
  if (n.blocked) {
    control = h("span", { class: "muted" }, "Can't join");
  } else if (typing) {
    control = h(
      "span",
      { class: "control" },
      h("input", {
        class: "text",
        type: "password",
        name: "password",
        autocomplete: "off",
        "aria-label": `Password for ${n.name}`,
        placeholder: "Password",
        disabled: busy,
        oninput: (e: Event) => state.joining && (state.joining.password = (e.target as HTMLInputElement).value),
        onkeydown: (e: Event) => {
          if ((e as KeyboardEvent).key === "Enter" && state.joining) void joinWifi(n.name, state.joining.password);
        },
      }),
      h("button", { class: "button", disabled: busy, "data-test": "wifi-join", onclick: () => state.joining && joinWifi(n.name, state.joining.password) }, "Join"),
      h("button", { class: "link", disabled: busy, onclick: () => ((state.joining = null), render()) }, "Cancel"),
    );
  } else {
    control = h(
      "span",
      { class: "control" },
      !n.joined && n.signal !== undefined
        ? h(
            "button",
            {
              class: "button",
              disabled: busy,
              "data-test": "wifi-join",
              onclick: () => {
                // A password the first time, unless it's open or the computer knows it.
                if (n.security === "personal" && !n.organization && (!n.known || n.failed)) {
                  state.joining = { network: n.name, password: "" };
                  render();
                } else void joinWifi(n.name, null);
              },
            },
            "Join",
          )
        : null,
      n.known && !n.organization ? h("button", { class: "link", disabled: busy, "data-test": "wifi-forget", onclick: () => forgetWifi(n.name) }, "Forget") : null,
    );
  }
  return h(
    "div",
    { class: "setting", "data-network": n.name },
    h(
      "div",
      {},
      h("div", { class: "title" }, h("span", { class: "bars", "aria-hidden": "true" }, bars(n.signal)), " ", n.name, n.security && n.security !== "open" ? " 🔒" : ""),
      notes.length ? h("div", { class: "help" }, notes.join(" · ")) : null,
      n.blocked ? h("div", { class: "who it" }, n.blocked) : null,
    ),
    control,
  );
}

/** Wi-Fi: the network in use and the ones around, inside IT's rules. */
function wifi(w: Wifi): HTMLElement {
  const rows: (HTMLElement | null)[] = w.present
    ? [
        w.only_organization ? h("div", { class: "setting" }, h("div", { class: "who it" }, "🔒 Your organization allows only its own networks on this computer")) : null,
        ...(w.networks.length ? w.networks.map((n) => network(w, n)) : [h("div", { class: "setting" }, h("div", { class: "help" }, "Looking for networks…"))]),
      ]
    : [h("div", { class: "setting" }, h("div", { class: "help" }, "This computer has no Wi-Fi."))];
  return h(
    "section",
    { class: "group", "data-section": "wifi" },
    h("h2", {}, "Wi-Fi"),
    h("div", { class: "rows" }, ...rows),
  );
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
  if (setting.kind.type === "sites" || setting.kind.type === "folders" || setting.kind.type === "networks") {
    const list = Array.isArray(setting.value) ? setting.value : [];
    const folders = setting.kind.type === "folders";
    const networks = setting.kind.type === "networks";
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
          placeholder: networks ? "One per line: Free Airport WiFi" : folders ? "One per line, in your home: Documents" : "One per line: games.example",
          disabled: busy,
          onchange: (e: Event) =>
            change(
              setting,
              (e.target as HTMLTextAreaElement).value
                // A network's name may have a comma in it.
                .split(networks ? /\n+/ : /[\n,]+/)
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
      state.wifi ? wifi(state.wifi) : null,
      ...sections.map((section) =>
        h(
          "section",
          { class: "group" },
          h("h2", {}, section),
          h(
            "div",
            { class: "rows" },
            section === DISPLAY && state.brightness?.present ? brightness(state.brightness) : null,
            ...(section === COMPUTER && state.computer ? computer(state.computer) : []),
            ...state.settings.filter((s) => s.section === section).map(row),
          ),
        ),
      ),
      state.models ? models(state.models) : null,
    ),
  );
  const page = root.querySelector(".page");
  if (page) page.scrollTop = scrolled;
}

void load();
// The networks around change as the person moves: looked at again every
// few seconds while Settings is open, unless they're typing a password.
setInterval(() => {
  if (state.busy || state.joining || document.hidden) return;
  // The slider being dragged isn't redrawn under the pointer.
  if (document.activeElement?.getAttribute("name") === "brightness") return;
  void Promise.all([loadWifi(), loadBrightness(), loadComputer()]).then(render);
}, 8000);
