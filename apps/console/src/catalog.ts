// The org's private catalog (AD-5, DV-10, DV-11), after the "Catalog ·
// approving an agent" mockup: what each item declares it can do (DV-3),
// and IT's decision per group: required, available or blocked. Approving
// or blocking signs a new catalog that reaches every device at once. The
// org's items never go to the public store.

import { h, show } from "@glasshouse/sdk";

import { type CatalogPage, type Item, type Mode, api } from "./api.ts";
import { capital, card, chip, mono, notice, plural } from "./ui.ts";

const REVIEW: Record<Item["review"], [string, "ok" | "wait" | "bad"]> = {
  waiting: ["Waiting for review", "wait"],
  approved: ["Approved", "ok"],
  blocked: ["Blocked", "bad"],
};

const MODES: [Mode, string][] = [
  ["available", "Available for self-install"],
  ["required", "Required: installed for them"],
  ["blocked", "Blocked for this group"],
];

export async function catalogPage(root: HTMLElement): Promise<void> {
  const page = await api<CatalogPage>("GET", "/api/catalog");
  const order = { waiting: 0, approved: 1, blocked: 2 };
  const items = [...page.items].sort((a, b) => order[a.review] - order[b.review] || a.name.localeCompare(b.name));
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
          h("div", { class: "crumbs" }, "Catalog"),
          h("h1", {}, "Catalog"),
          h(
            "p",
            { class: "sub" },
            `${page.counts.waiting} waiting for review · ${page.counts.approved} approved · ${page.counts.blocked} blocked · `,
            mono(page.version ? `catalog v${page.version} on devices` : "nothing published yet"),
          ),
        ),
      ),
      card(
        null,
        null,
        h(
          "table",
          { class: "list" },
          h("thead", {}, h("tr", {}, ...["Item", "Publisher", "Version", "Status", "Groups"].map((t) => h("th", {}, t)))),
          h(
            "tbody",
            {},
            ...items.map((i) =>
              h(
                "tr",
                { class: "link", "data-item": i.id, onclick: () => (location.hash = `#/catalog/${i.id}`) },
                h("td", {}, h("a", { href: `#/catalog/${i.id}` }, i.name), h("div", { class: "hint" }, capital(i.kind_name))),
                h("td", {}, i.publisher, i.verified ? h("span", { class: "verified" }, " ✓") : null),
                h("td", {}, mono(`v${i.version}`)),
                h("td", {}, chip(REVIEW[i.review][0], REVIEW[i.review][1])),
                h("td", { class: "hint" }, i.groups.map((g) => `${g.display}: ${g.mode}`).join(", ") || "–"),
              ),
            ),
          ),
        ),
      ),
    ),
  );
}

export async function itemPage(root: HTMLElement, id: string): Promise<void> {
  const [page, item] = await Promise.all([
    api<CatalogPage>("GET", "/api/catalog"),
    api<Item>("GET", `/api/catalog/${encodeURIComponent(id)}`),
  ]);
  let message: { text: string; tone: "ok" | "bad" } | null = null;
  const draw = (it: Item) => show(root, render(page, it, message, act));
  async function act(work: () => Promise<string>): Promise<void> {
    try {
      message = { text: await work(), tone: "ok" };
    } catch (error) {
      message = { text: (error as Error).message, tone: "bad" };
    }
    draw(await api<Item>("GET", `/api/catalog/${encodeURIComponent(id)}`));
  }
  draw(item);
}

function render(
  page: CatalogPage,
  item: Item,
  message: { text: string; tone: "ok" | "bad" } | null,
  act: (work: () => Promise<string>) => Promise<void>,
): HTMLElement {
  const groups = page.groups;
  const requested = groups.find((g) => g.display === item.requested_by)?.name ?? groups[1]?.name ?? "everyone";
  const groupPick = h(
    "select",
    { "data-test": "group", "aria-label": "Approve for group" },
    ...groups.map((g) => h("option", { value: g.name, selected: g.name === requested }, `${g.display} (${plural(g.users, "user")})`)),
  );
  const modePick = h(
    "select",
    { "data-test": "mode", "aria-label": "Install mode" },
    ...MODES.map(([m, label]) => h("option", { value: m }, label)),
  );
  const pin = h("input", { type: "checkbox", checked: item.pinned, "data-test": "pin" });
  const declares = item.declares;
  const network = declares.egress.length ? declares.egress.join(", ") : "Model gateway only";
  return h(
    "div",
    { class: "page" },
    h(
      "header",
      { class: "page-head" },
      h(
        "div",
        {},
        h("div", { class: "crumbs" }, h("a", { href: "#/catalog" }, "Catalog"), ` / ${REVIEW[item.review][0]}`),
        h("h1", {}, item.name),
        h(
          "p",
          { class: "sub" },
          item.publisher,
          item.verified ? h("span", { class: "verified" }, " ✓ Verified publisher") : " · publisher not verified",
          " ",
          mono(`v${item.version}`),
          item.requested_by ? ` Requested by ${item.requested_by}` : null,
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
        card(
          "What it can do",
          "Declared by the publisher and enforced by the OS. It can never go beyond this.",
          ...declares.actions.map((a) => h("div", { class: "kv" }, h("span", {}, a.title), h("span", {}, a.tier ? capital(a.tier) : "Policy decides"))),
          h("div", { class: "kv" }, h("span", {}, `Data up to ${capital(declares.max_label)}`), h("span", {}, "Routed by the org's model rules")),
          h("div", { class: "kv" }, h("span", {}, "Network access"), h("span", { class: "strong" }, network)),
        ),
        card(
          "Checks",
          "What the submission's automated checks reported (DV-2).",
          item.checks.length ? h("div", { class: "chips" }, ...item.checks.map((c) => chip(c, "ok"))) : h("p", { class: "hint" }, "None reported."),
        ),
        card(
          "Who gets it",
          item.groups.length ? null : "No group yet.",
          ...item.groups.map((g) => h("div", { class: "kv" }, h("span", {}, `${g.display} (${plural(g.users, "user")})`), chip(capital(g.mode), g.mode === "blocked" ? "bad" : "ok"))),
          h(
            "p",
            { class: "hint" },
            item.internal ? `Built by the org and signed with its key. ` : "",
            item.public_store ? `Never listed publicly: ${item.public_store}.` : "",
          ),
        ),
      ),
      h(
        "aside",
        { class: "side" },
        card(
          "Your decision",
          null,
          h("label", { class: "field" }, h("span", {}, "Approve for group"), groupPick),
          h("label", { class: "field" }, h("span", {}, "Install mode"), modePick),
          h("label", { class: "check-row inline" }, pin, h("span", {}, `Pin to v${item.version}`)),
          h(
            "button",
            {
              class: "button primary wide",
              "data-test": "approve",
              onclick: () =>
                act(async () => {
                  const r = await api<{ catalog_version: number }>("POST", `/api/catalog/${item.id}/approve`, {
                    group: groupPick.value,
                    mode: modePick.value,
                    pin: pin.checked,
                  });
                  return `Approved. Catalog v${r.catalog_version} is on its way to every device.`;
                }),
            },
            "Approve",
          ),
          h(
            "button",
            {
              class: "button wide",
              "data-test": "block",
              onclick: () =>
                act(async () => {
                  const r = await api<{ catalog_version: number }>("POST", `/api/catalog/${item.id}/block`, {});
                  return `Blocked. Catalog v${r.catalog_version} leaves it out on every device.`;
                }),
            },
            "Block",
          ),
          h("p", { class: "hint" }, "Your decision is recorded with your name and goes to devices signed with the org's key."),
        ),
      ),
    ),
  );
}
