// Settings (UX-1): every device setting, laid out like the computer's own
// Settings app, for one group at a time. Each is left to the next group,
// set as a default people can change, or locked. Settings are policy: saved
// to the draft, previewed, published to the pilot ring and promoted like
// any rule. When someone's groups disagree, the group higher in the list
// wins; the browser's settings are the whole computer's, so only Everyone
// sets them.

import { h, show } from "@glasshouse/sdk";

import { type Impact, type Managed, type SettingRow, type SettingValue, type SettingsPage, api } from "./api.ts";
import { card, chip, mono, notice, plural } from "./ui.ts";

/** Time zones IT picks from most; any other name of the zone database works too. */
const ZONES = [
  "UTC",
  "America/New_York",
  "America/Chicago",
  "America/Denver",
  "America/Los_Angeles",
  "America/Sao_Paulo",
  "Europe/London",
  "Europe/Paris",
  "Europe/Berlin",
  "Asia/Kolkata",
  "Asia/Singapore",
  "Asia/Tokyo",
  "Australia/Sydney",
];

type State = "unset" | "default" | "locked";

/** A value in words, as the preview says it. */
function words(row: SettingRow, value: SettingValue): string {
  if (typeof value === "boolean") return value ? "On" : "Off";
  if (typeof value === "number") return row.kind.type === "number" ? `${value} ${row.kind.unit}` : String(value);
  if (Array.isArray(value)) return value.length ? (value.length <= 3 ? value.join(", ") : plural(value.length, "site")) : "None";
  return value || "None";
}

export async function settingsPage(root: HTMLElement, group: string): Promise<void> {
  let page = await api<SettingsPage>("GET", `/api/settings?group=${encodeURIComponent(group)}`);
  let impact: Impact | null = null;
  let message: { text: string; tone: "ok" | "bad" } | null = null;
  // The form: each setting's state and value, from the draft.
  const form = new Map<string, { state: State; value: SettingValue }>();
  const load = () => {
    form.clear();
    for (const row of page.settings) {
      form.set(row.id, row.set ? { state: row.set.locked ? "locked" : "default", value: row.set.value } : { state: "unset", value: row.everyone?.value ?? row.default });
    }
  };
  load();

  async function act(work: () => Promise<string | null>): Promise<void> {
    try {
      const text = await work();
      message = text ? { text, tone: "ok" } : null;
    } catch (error) {
      message = { text: (error as Error).message, tone: "bad" };
    }
    page = await api<SettingsPage>("GET", `/api/settings?group=${encodeURIComponent(group)}`);
    load();
    draw();
  }

  function save(): Promise<void> {
    return act(async () => {
      const settings: Record<string, Managed | null> = {};
      for (const row of page.settings) {
        const f = form.get(row.id);
        if (!f) continue;
        settings[row.id] = f.state === "unset" ? null : { value: f.value, locked: f.state === "locked" };
      }
      await api("POST", "/api/policy/edit", { edit: "settings", group, settings });
      impact = await api<Impact>("POST", "/api/policy/preview", {});
      return "Saved to the draft. Preview, then publish to the pilot ring.";
    });
  }

  function move(name: string, by: -1 | 1): Promise<void> {
    const order = [...page.order];
    const at = order.indexOf(name);
    const to = at + by;
    if (at < 0 || to < 0 || to >= order.length) return Promise.resolve();
    [order[at], order[to]] = [order[to] as string, order[at] as string];
    return act(async () => {
      await api("POST", "/api/policy/edit", { edit: "group_order", order });
      return "Group order saved to the draft.";
    });
  }

  function control(row: SettingRow, f: { state: State; value: SettingValue }, disabled: boolean): HTMLElement {
    const set = (value: SettingValue) => (f.value = value);
    switch (row.kind.type) {
      case "toggle":
        return h("input", { type: "checkbox", name: row.id, checked: f.value === true, disabled, onchange: (e: Event) => set((e.target as HTMLInputElement).checked) });
      case "number":
        return h(
          "span",
          { class: "unit" },
          h("input", {
            class: "text short",
            name: row.id,
            inputmode: "numeric",
            value: String(f.value),
            disabled,
            oninput: (e: Event) => set(Number((e.target as HTMLInputElement).value) || 0),
          }),
          ` ${row.kind.unit}`,
        );
      case "url":
        return h("input", { class: "text", name: row.id, value: String(f.value), placeholder: "https://intranet.example", disabled, oninput: (e: Event) => set((e.target as HTMLInputElement).value.trim()) });
      case "sites":
        return h(
          "textarea",
          {
            class: "text",
            name: row.id,
            rows: 3,
            placeholder: "One per line: games.example, *.social.example",
            disabled,
            oninput: (e: Event) =>
              set(
                (e.target as HTMLTextAreaElement).value
                  .split(/[\n,]+/)
                  .map((s) => s.trim())
                  .filter(Boolean),
              ),
          },
          Array.isArray(f.value) ? f.value.join("\n") : "",
        );
      case "time_zone":
        return h(
          "span",
          {},
          h("input", {
            class: "text",
            name: row.id,
            value: String(f.value),
            placeholder: "America/New_York",
            list: "time-zones",
            disabled,
            oninput: (e: Event) => set((e.target as HTMLInputElement).value.trim()),
          }),
          h("datalist", { id: "time-zones" }, ...ZONES.map((zone) => h("option", { value: zone }))),
        );
    }
  }

  function settingRow(row: SettingRow): HTMLElement {
    const f = form.get(row.id) ?? { state: "unset" as State, value: row.default };
    // The time zone is the computer's whoever uses it; the browser's and
    // USB drives follow whoever is signed in (ADR 0031).
    const computerWide = row.scope === "device" && group !== "everyone";
    const forSeat = row.scope === "seat" && group !== "everyone";
    const canChange =
      row.scope === "person"
        ? "People can change it in Settings."
        : row.id.startsWith("browser.")
          ? "People can change it in the browser."
          : "The computer applies it; people can't change it.";
    const fallback = row.everyone ? `Everyone's: ${words(row, row.everyone.value)}${row.everyone.locked ? ", locked" : ""}` : `Computer's default: ${words(row, row.default)}`;
    const states: [State, string][] = [
      ["unset", group === "everyone" ? "Not set" : "Not set here"],
      ["default", "Default"],
      ["locked", "Locked"],
    ];
    return h(
      "div",
      { class: "setting", "data-setting": row.id },
      h(
        "div",
        { class: "setting-text" },
        h("div", { class: "strong" }, row.title),
        h("div", { class: "hint" }, row.help),
        computerWide
          ? h("div", { class: "hint" }, "For the whole computer: set it for Everyone.")
          : f.state === "unset"
            ? h("div", { class: "hint" }, fallback)
            : f.state === "default"
              ? h("div", { class: "hint" }, canChange)
              : h("div", { class: "hint" }, "People see it but can't change it."),
        forSeat ? h("div", { class: "hint", "data-test": "for-seat" }, "On the computer while someone in this group is signed in.") : null,
      ),
      computerWide
        ? h("div", {}, row.everyone ? chip(`Everyone: ${words(row, row.everyone.value)}`) : chip("Not set"))
        : h(
            "div",
            { class: "setting-edit" },
            h(
              "div",
              { class: "segmented three", role: "radiogroup", "aria-label": `${row.title}: how it's set` },
              ...states.map(([s, label]) =>
                h(
                  "button",
                  {
                    class: `seg${s === "locked" ? " never" : ""}${f.state === s ? " on" : ""}`,
                    role: "radio",
                    "aria-checked": f.state === s ? "true" : "false",
                    "data-state": s,
                    onclick: () => {
                      f.state = s;
                      draw();
                    },
                  },
                  label,
                ),
              ),
            ),
            control(row, f, f.state === "unset"),
          ),
    );
  }

  function draw(): void {
    const sections = [...new Set(page.settings.map((r) => r.section))];
    const inOrder = new Set(page.order);
    show(
      root,
      h(
        "div",
        { class: "page" },
        h(
          "header",
          { class: "page-head" },
          h(
            "div",
            {},
            h("div", { class: "crumbs" }, "Settings"),
            h("h1", {}, `Settings: ${page.group.display}`),
            h("p", { class: "sub" }, `${plural(page.group.people, "person", "people")}. What their computers are set to; locked settings can't be changed on the computer.`),
          ),
          h(
            "div",
            { class: "actions" },
            h(
              "button",
              {
                class: "button",
                "data-test": "preview",
                onclick: () =>
                  act(async () => {
                    impact = await api<Impact>("POST", "/api/policy/preview", {});
                    return null;
                  }),
              },
              "Preview impact",
            ),
            h(
              "button",
              {
                class: "button primary",
                "data-test": "publish",
                disabled: !page.changed,
                onclick: () =>
                  act(async () => {
                    const r = await api<{ version: number }>("POST", "/api/policy/publish", {
                      note: impact?.changes.find((c) => c.startsWith(page.group.display)) ?? `Settings for ${page.group.display}`,
                    });
                    impact = null;
                    return `Published v${r.version} to the pilot ring. Pilot computers apply it in about a second.`;
                  }),
              },
              "Publish to pilot ring",
            ),
          ),
        ),
        message ? notice(message.text, message.tone) : null,
        h(
          "div",
          { class: "columns" },
          h(
            "div",
            { class: "main" },
            ...sections.map((section) => card(section, null, ...page.settings.filter((r) => r.section === section).map(settingRow))),
            h("div", { class: "actions" }, h("button", { class: "button primary", "data-test": "save-settings", onclick: () => void save() }, "Save to draft")),
          ),
          h(
            "aside",
            { class: "side" },
            card(
              "Groups",
              "When someone's groups disagree, the higher one wins. Everyone comes last.",
              h(
                "div",
                { class: "group-list", "data-test": "groups" },
                ...page.groups.map((g) =>
                  h(
                    "div",
                    { class: `group-row${g.name === page.group.name ? " current" : ""}` },
                    h("a", { href: `#/settings/${encodeURIComponent(g.name)}` }, g.display),
                    h("span", { class: "hint" }, `${g.people}${g.sets ? ` · ${g.sets} set` : ""}`),
                    inOrder.has(g.name)
                      ? h(
                          "span",
                          { class: "order" },
                          h("button", { class: "icon-button", "aria-label": `Move ${g.display} up`, onclick: () => void move(g.name, -1) }, "↑"),
                          h("button", { class: "icon-button", "aria-label": `Move ${g.display} down`, onclick: () => void move(g.name, 1) }, "↓"),
                        )
                      : null,
                  ),
                ),
              ),
            ),
            card(
              "Versions and rollout",
              null,
              h("div", { class: "kv" }, h("span", {}, "Pilot ring"), mono(page.pilot_version ? `v${page.pilot_version}` : "–")),
              h("div", { class: "kv" }, h("span", {}, "Draft"), page.changed ? chip("Changed", "wait") : chip("Same as pilot", "ok")),
            ),
            impact
              ? card(
                  "Preview impact",
                  impact.base_version ? `Compared with v${impact.base_version}, which every device runs.` : null,
                  h("ul", { class: "impact", "data-test": "impact" }, ...impact.changes.map((c) => h("li", {}, c))),
                  ...impact.warnings.map((w) => h("p", { class: "warning small" }, w)),
                )
              : null,
          ),
        ),
      ),
    );
  }
  draw();
}
