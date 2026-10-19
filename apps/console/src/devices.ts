// The fleet (AD-4): every device's last report, by ring (AD-11), and
// what a device needs to enroll (AD-10, a development stub). Counts and
// names only: devices never send task text or paths.

import { h, show } from "@glasshouse/sdk";

import { type FleetPage, type Ring, api } from "./api.ts";
import { ago, capital, card, chip, mono, notice, plural, stat } from "./ui.ts";

const RINGS: Ring[] = ["pilot", "early", "broad"];

export async function devicesPage(root: HTMLElement): Promise<void> {
  let message: string | null = null;
  const draw = (page: FleetPage) => show(root, render(page, message, setRing, setKind));
  async function change(device: string, what: "ring" | "kind", value: string | null): Promise<void> {
    try {
      await api("POST", `/api/fleet/${encodeURIComponent(device)}/${what}`, { [what]: value });
      message = what === "ring" ? `${device} is in the ${value} ring now.` : value ? `${device} is a ${value} now.` : `${device} has no kind now.`;
    } catch (error) {
      message = (error as Error).message;
    }
    draw(await api<FleetPage>("GET", "/api/fleet"));
  }
  const setRing = (device: string, ring: Ring) => change(device, "ring", ring);
  const setKind = (device: string, kind: string | null) => change(device, "kind", kind);
  draw(await api<FleetPage>("GET", "/api/fleet"));
}

function render(
  page: FleetPage,
  message: string | null,
  setRing: (device: string, ring: Ring) => Promise<void>,
  setKind: (device: string, kind: string | null) => Promise<void>,
): HTMLElement {
  const t = page.totals;
  return h(
    "div",
    { class: "page" },
    h(
      "header",
      { class: "page-head" },
      h(
        "div",
        {},
        h("div", { class: "crumbs" }, "Devices"),
        h("h1", {}, "Devices"),
        h("p", { class: "sub" }, `${plural(t.devices, "device")} · ${plural(t.users, "person", "people")} · `, mono(page.catalog_version ? `catalog v${page.catalog_version}` : "no catalog yet")),
      ),
    ),
    message ? notice(message) : null,
    h(
      "div",
      { class: "stats" },
      card(null, null, stat("Online now", t.online)),
      card(null, null, stat("Agents running", t.running, "blue")),
      card(null, null, stat("Waiting on a person", t.waiting, "orange")),
      card(null, null, stat("Blocked by policy, 24h", t.blocked_24h)),
      ...page.rings.map((r) => card(null, null, stat(`${capital(r.ring)} ring · ${plural(r.devices, "device")}`, r.policy_version ? `v${r.policy_version}` : "–"))),
    ),
    h(
      "div",
      { class: "columns" },
      h(
        "div",
        { class: "main" },
        card(
          null,
          null,
          h(
            "table",
            { class: "list devices" },
            h(
              "thead",
              {},
              h(
                "tr",
                {},
                ...["Device", "People", "Ring", "Kind", "Policy", "Catalog"].map((x) => h("th", {}, x)),
                ...["Agents", "Waiting", "Blocked 24h"].map((x) => h("th", { class: "num" }, x)),
                h("th", {}, "Last seen"),
              ),
            ),
            h(
              "tbody",
              {},
              ...page.devices.map((d) =>
                h(
                  "tr",
                  { "data-device": d.id },
                  h("td", {}, h("span", { class: `dot ${d.online ? "green" : "grey"}` }), mono(d.name), d.last_error ? h("div", { class: "warning small" }, d.last_error) : null),
                  h("td", {}, d.users.join(", ") || "–"),
                  h(
                    "td",
                    {},
                    h(
                      "select",
                      {
                        "aria-label": `Ring for ${d.name}`,
                        onchange: (e: Event) => setRing(d.id, (e.target as HTMLSelectElement).value as Ring),
                      },
                      ...RINGS.map((r) => h("option", { value: r, selected: r === d.ring }, capital(r))),
                    ),
                  ),
                  h(
                    "td",
                    {},
                    page.kinds.length
                      ? h(
                          "select",
                          {
                            "aria-label": `Kind of computer ${d.name} is`,
                            name: "kind",
                            onchange: (e: Event) => setKind(d.id, (e.target as HTMLSelectElement).value || null),
                          },
                          h("option", { value: "", selected: !d.kind }, "–"),
                          ...page.kinds.map((k) => h("option", { value: k, selected: k === d.kind }, capital(k))),
                        )
                      : "–",
                  ),
                  h("td", {}, d.policy_version ? mono(`v${d.policy_version}`) : "–"),
                  h("td", {}, d.catalog_version ? mono(`v${d.catalog_version}`) : "–"),
                  h("td", { class: "num" }, String(d.agents)),
                  h("td", { class: "num" }, d.waiting ? chip(String(d.waiting), "wait") : "0"),
                  h("td", { class: "num" }, String(d.blocked_24h)),
                  h("td", { class: "hint" }, ago(d.last_seen)),
                ),
              ),
            ),
          ),
        ),
      ),
      h(
        "aside",
        { class: "side" },
        card(
          "Enroll a device",
          "Zero-touch enrollment isn't built yet (AD-10). A device enrolls with these in its system settings and the org's enrollment token.",
          h("div", { class: "kv stack" }, h("span", {}, "Console"), mono(page.enroll.url)),
          h("div", { class: "kv stack" }, h("span", {}, "Org"), mono(page.enroll.org)),
          h("div", { class: "kv stack" }, h("span", {}, "Org key"), mono(page.enroll.org_key)),
        ),
      ),
    ),
  );
}
