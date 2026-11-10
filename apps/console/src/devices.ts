// The fleet (AD-4): every device's last report, by ring (AD-11), and
// what a device needs to enroll (AD-10, a development stub). Counts and
// names only: devices never send task text or paths.

import { h, show } from "@glasshouse/sdk";

import { type DeviceRow, type FleetPage, type Ring, api } from "./api.ts";
import { ago, capital, card, carryOut, chip, mono, notice, plural, SIGNED_OUT, stat } from "./ui.ts";

const RINGS: Ring[] = ["pilot", "early", "broad"];

export async function devicesPage(root: HTMLElement): Promise<void> {
  let message: string | null = null;
  // The computer a Remove is waiting on a second click for.
  let confirming: string | null = null;
  let last: FleetPage | null = null;
  const draw = (page: FleetPage) => {
    last = page;
    show(root, render(page, message, setRing, setKind, { confirming, ask, remove, forget }, hold));
  };
  // An action, then the fleet read again and drawn (ui.ts, carryOut).
  async function after(work: () => Promise<string>): Promise<void> {
    let fleet = last;
    const said = await carryOut(work, async () => {
      fleet = await api<FleetPage>("GET", "/api/fleet");
    });
    if (said === SIGNED_OUT) return;
    message = said?.text ?? null;
    if (fleet) draw(fleet);
  }
  function change(device: string, what: "ring" | "kind", value: string | null): Promise<void> {
    return after(async () => {
      await api("POST", `/api/fleet/${encodeURIComponent(device)}/${what}`, { [what]: value });
      return what === "ring" ? `${device} is in the ${value} ring now.` : value ? `${device} is a ${value} now.` : `${device} has no kind now.`;
    });
  }
  const setRing = (device: string, ring: Ring) => change(device, "ring", ring);
  const setKind = (device: string, kind: string | null) => change(device, "kind", kind);
  // Removing a computer (ADR 0030 slice P) asks once more, on its row.
  const ask = (device: string | null) => {
    confirming = device;
    if (last) draw(last);
  };
  function act(device: string, name: string, what: "revoke" | "forget"): Promise<void> {
    confirming = null;
    return after(async () => {
      await api("POST", `/api/fleet/${encodeURIComponent(device)}/${what}`, {});
      return what === "revoke"
        ? `${name} was removed from the organization. It can't reach the console until it enrolls again.`
        : `${name} is forgotten. Set up again, it can enroll under the same name.`;
    });
  }
  // IT holds OS updates back from a ring, or lets them go to it again (ADR 0061).
  function hold(ring: Ring, held: boolean): Promise<void> {
    return after(async () => {
      await api("POST", "/api/updates/hold", { ring, hold: held });
      return held
        ? `OS updates are held back from the ${ring} ring. Its computers hear of no release and download none until you resume.`
        : `OS updates go to the ${ring} ring again.`;
    });
  }
  const remove = (device: string, name: string) => act(device, name, "revoke");
  const forget = (device: string, name: string) => act(device, name, "forget");
  draw(await api<FleetPage>("GET", "/api/fleet"));
}

/** Removing a computer from the organization, from its row. */
interface Removing {
  confirming: string | null;
  ask: (device: string | null) => void;
  remove: (device: string, name: string) => Promise<void>;
  forget: (device: string, name: string) => Promise<void>;
}

/** The row's last cell: Remove, its confirmation, or a removed computer's
 * Forget. */
function removal(d: DeviceRow, r: Removing): HTMLElement {
  if (d.removed) {
    return h(
      "td",
      { class: "row-actions" },
      chip("Removed", "bad"),
      h("button", { class: "button small", "data-test": "forget", title: "Let a computer set up again enroll under this name", onclick: () => void r.forget(d.id, d.name) }, "Forget"),
    );
  }
  if (r.confirming === d.id) {
    return h(
      "td",
      { class: "row-actions" },
      h("span", { class: "hint" }, `Remove ${d.name}? It stops syncing at once.`),
      h("button", { class: "button small danger", "data-test": "confirm-remove", onclick: () => void r.remove(d.id, d.name) }, "Remove"),
      h("button", { class: "button small", onclick: () => r.ask(null) }, "Cancel"),
    );
  }
  return h("td", { class: "row-actions" }, h("button", { class: "button small", "data-test": "remove", "aria-label": `Remove ${d.name} from the organization`, onclick: () => r.ask(d.id) }, "Remove…"));
}

function render(
  page: FleetPage,
  message: string | null,
  setRing: (device: string, ring: Ring) => Promise<void>,
  setKind: (device: string, kind: string | null) => Promise<void>,
  removing: Removing,
  hold: (ring: Ring, held: boolean) => Promise<void>,
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
            "div",
            { class: "table-scroll" },
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
                  h("th", {}, ""),
                ),
              ),
              h(
                "tbody",
                {},
                ...page.devices.map((d) =>
                  h(
                    "tr",
                    { "data-device": d.id },
                    h(
                      "td",
                      {},
                      h("span", { class: `dot ${d.online ? "green" : "grey"}` }),
                      mono(d.name),
                      // The OS it runs (ADR 0061), under its name.
                      d.os_version ? h("div", { class: "hint small", "data-test": "os-version" }, d.os_version) : null,
                      d.last_error ? h("div", { class: "warning small" }, d.last_error) : null,
                    ),
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
                    removal(d, removing),
                  ),
                ),
              ),
            ),
          ),
        ),
        made(page),
      ),
      h(
        "aside",
        { class: "side" },
        osUpdates(page, hold),
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

/** The OS release each ring's computers are offered (ADR 0061): the ring a
 * computer is in is the channel it updates on. Computers check every
 * release's signatures themselves; the console only passes them on. */
function osUpdates(page: FleetPage, hold: (ring: Ring, held: boolean) => Promise<void>): HTMLElement {
  const u = page.updates;
  if (!u) return card("OS updates", "This console serves no OS updates: computers take them from where their settings say.");
  const size = (bytes: number) => (bytes >= 1e9 ? `${(bytes / 1e9).toFixed(1)} GB` : `${Math.max(1, Math.round(bytes / 1e6))} MB`);
  return h(
    "section",
    { class: "card", "data-test": "os-updates" },
    h("h2", {}, "OS updates"),
    h("p", { class: "card-sub" }, "A computer's ring is the channel it updates on. Each computer checks a release's signatures itself before taking it."),
    ...u.rings.map((r) =>
      h(
        "div",
        { class: "kv stack ring-update", "data-ring": r.ring },
        h("span", {}, `${capital(r.ring)} ring`, r.held ? chip("Held", "wait") : null),
        r.version && r.number !== null && r.bytes !== null
          ? h(
              "span",
              {},
              mono(r.version),
              ` · release ${r.number}`,
              h("span", { class: "hint" }, r.delta_from ? `${size(r.bytes)} of changes from ${r.delta_from}` : `${size(r.bytes)}, the whole image`),
            )
          : h("span", { class: "hint" }, "Nothing offered"),
        r.held
          ? h("span", { class: "hint" }, `Held back by ${r.held.by} ${ago(r.held.since)}: its computers hear of no release and download none.`)
          : null,
        h(
          "button",
          {
            class: "button small",
            "data-test": r.held ? "resume-updates" : "hold-updates",
            title: r.held ? "Let this ring's computers take its release" : "Stop this ring's computers taking a release, downloads under way included",
            onclick: () => void hold(r.ring, !r.held),
          },
          r.held ? "Resume" : "Hold",
        ),
      ),
    ),
    u.mirror
      ? h(
          "p",
          { class: u.mirror.error ? "warning small" : "hint" },
          u.mirror.error
            ? `Mirroring ${u.mirror.from} failed${u.mirror.at ? ` ${ago(u.mirror.at)}` : ""}: ${u.mirror.error}`
            : `Mirrored from ${u.mirror.from}${u.mirror.at ? `, checked ${ago(u.mirror.at)}` : ""}.`,
        )
      : null,
  );
}

/** Every app people made on the org's computers (GA-10, ADR 0032): each
 * one is theirs alone until they share it through the store, and none is
 * hidden from IT. What each does beyond the app it remixes is the part to
 * look at. */
function made(page: FleetPage): HTMLElement {
  return card(
    "Apps people made",
    page.made.length
      ? `${plural(page.made.length, "app")}, each its maker's alone until it's shared through the catalog.`
      : "Nobody has made or remixed an app yet. Each one shows here, whoever made it.",
    page.made.length
      ? h(
          "table",
          { class: "list made", "data-test": "made" },
          h(
            "thead",
            {},
            h("tr", {}, ...["App", "Made by", "Based on", "Version", "Beyond its base", "Computer"].map((x) => h("th", {}, x))),
          ),
          h(
            "tbody",
            {},
            ...page.made.map((m) =>
              h(
                "tr",
                { "data-app": m.app },
                h("td", {}, m.name, " ", mono(m.app)),
                h("td", {}, m.owner),
                h("td", {}, m.based_on ? mono(m.based_on) : "new"),
                h("td", {}, mono(`v${m.version}`)),
                h("td", {}, m.beyond_base.length ? chip(m.beyond_base.join(", "), "wait") : m.based_on ? "nothing" : `${plural(m.actions.length, "action")}`),
                h("td", {}, mono(m.device)),
              ),
            ),
          ),
        )
      : null,
  );
}
