// The org's private catalog (AD-5, DV-10, DV-11), after the "Catalog ·
// approving an agent" mockup: what each item declares it can do (DV-3),
// and IT's decision per group: required, available or blocked. Approving
// or blocking signs a new catalog that reaches every device at once. The
// org's items never go to the public store.
//
// An app someone at the org made and shared (ADR 0032, R2; GA-8) is
// reviewed here: who made it from what, the console's own checks, what it
// does beyond the app it remixes, what changed file by file, and whether a
// person must review it (DV-4).

import { h, show } from "@glasshouse/sdk";

import { type CatalogPage, type FileChange, type Item, type Mode, type Shared, type Titled, api } from "./api.ts";
import { ago, capital, card, chip, mono, notice, plural } from "./ui.ts";

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
  const shared = item.shared ?? null;
  // A shared app goes where its maker asked: their team, or everyone.
  const requested = shared
    ? shared.to === "team" && shared.group
      ? shared.group
      : "everyone"
    : (groups.find((g) => g.display === item.requested_by)?.name ?? groups[1]?.name ?? "everyone");
  const base = shared?.based_on ? baseName(page, shared.based_on.id) : null;
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
        shared
          ? madeBy(item, shared, base)
          : h(
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
    shared?.needs_person ? needsPerson(shared.irreversible) : null,
    h(
      "div",
      { class: "columns" },
      h(
        "div",
        { class: "main" },
        ...(shared ? review(item, shared, base) : []),
        card(
          "What it can do",
          "Declared by the publisher and enforced by the OS. It can never go beyond this.",
          ...declares.actions.map((a) => h("div", { class: "kv" }, h("span", {}, a.title), h("span", {}, a.tier ? capital(a.tier) : "Policy decides"))),
          h("div", { class: "kv" }, h("span", {}, `Data up to ${capital(declares.max_label)}`), h("span", {}, "Routed by the org's model rules")),
          h("div", { class: "kv" }, h("span", {}, "Network access"), h("span", { class: "strong" }, network)),
        ),
        // A shared app's checks are the console's own, above.
        shared
          ? null
          : card(
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
          item.approved_version
            ? h("p", { class: "kept", "data-test": "kept-version" }, `Computers keep version ${item.approved_version} until you decide.`)
            : null,
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

// ---- a shared app's review (ADR 0032, R2; GA-8) ----------------------------

const STATUS: Record<FileChange["status"], [string, "ok" | "bad" | "plain"]> = {
  added: ["Added", "ok"],
  removed: ["Removed", "bad"],
  changed: ["Changed", "plain"],
};

/** Lines of one file's diff the page draws; the rest is counted. */
const MAX_DIFF_LINES = 1500;

/** What IT calls the app it remixes: its catalog name if it has one, else its id. */
function baseName(page: CatalogPage, id: string): string {
  return page.items.find((i) => i.id === id)?.name ?? id;
}

/** "Made by Dana Ruiz from mail 0.1.0 · for AP Operations". */
function madeBy(item: Item, shared: Shared, base: string | null): HTMLElement {
  const from = shared.based_on && base ? `from ${base} ${shared.based_on.version}` : "as a new app";
  const audience = shared.to === "team" ? (shared.group_name ?? shared.group ?? "their team") : "everyone in the organization";
  return h("p", { class: "sub", "data-test": "made-by" }, `Made by ${shared.maker} ${from} · for ${audience} `, mono(`v${item.version}`));
}

/** DV-4: what it can do that can't be undone, so a person reviews it. */
function needsPerson(irreversible: Titled[]): HTMLElement {
  return h(
    "div",
    { class: "dv4", role: "note", "data-test": "needs-person" },
    h("strong", {}, "A person must review this: "),
    `it can ${sentence(irreversible.map((a) => lower(a.title)))}. Approving it is that review.`,
  );
}

function review(item: Item, shared: Shared, base: string | null): HTMLElement[] {
  return [
    tagged(
      card(
        "The maker's note",
        `${shared.maker} shared their version ${shared.maker_version} from ${shared.device}, ${ago(shared.submitted_at)}.`,
        h("blockquote", { class: "maker-note" }, shared.note),
      ),
      "note",
    ),
    beyond(shared, base),
    checks(shared),
    changed(item.changes ?? []),
  ];
}

/** The privilege delta: what it may do that the app it remixes may not. */
function beyond(shared: Shared, base: string | null): HTMLElement {
  const more = shared.beyond_base;
  const [title, sub] = base
    ? more.length
      ? [`Does more than ${base}`, `What it may do that ${base} may not.`]
      : [`Nothing beyond ${base}`, `It may do only what ${base} may.`]
    : ["Everything it can do", "A new app: every action it declares is new."];
  return tagged(
    card(title, sub, ...more.map((a) => h("div", { class: "kv", "data-action": a.action }, h("span", {}, a.title), mono(a.action)))),
    "beyond",
  );
}

/** The console's own checks, then what the computer's noted. */
function checks(shared: Shared): HTMLElement {
  return tagged(
    card(
      "The console's checks",
      shared.passed
        ? "Run again here against the actions computers offer, and built here: every check passed."
        : "Run again here against the actions computers offer: not every check passed.",
      h(
        "ul",
        { class: "checks" },
        ...shared.checks.map((c) =>
          h(
            "li",
            { class: c.ok ? "ok" : "bad", "data-test": "check", "data-ok": String(c.ok) },
            h("span", { class: "mark", "aria-hidden": "true" }, c.ok ? "✓" : "✗"),
            h("span", { class: "sr" }, c.ok ? "Passed: " : "Failed: "),
            h("span", {}, c.text),
          ),
        ),
      ),
      shared.device_notes.length
        ? h(
            "div",
            { class: "device-notes", "data-test": "device-notes" },
            h("p", { class: "hint" }, `What ${shared.device}'s own checks noted:`),
            h("ul", {}, ...shared.device_notes.map((n) => h("li", {}, n))),
          )
        : h("p", { class: "hint" }, `${shared.device}'s own checks noted nothing.`),
    ),
    "checks",
  );
}

/** What changed from the base the console holds, file by file. */
function changed(changes: FileChange[]): HTMLElement {
  const added = changes.reduce((n, c) => n + c.added, 0);
  const removed = changes.reduce((n, c) => n + c.removed, 0);
  return tagged(
    card(
      "What changed",
      changes.length
        ? `${plural(changes.length, "file")}, +${added} −${removed}, compared with the console's own copy of what it remixes.`
        : "Nothing: it's the same as what it remixes.",
      ...changes.map((c) => fileDiff(c, changes.length === 1)),
    ),
    "changes",
  );
}

function fileDiff(change: FileChange, open: boolean): HTMLElement {
  const [status, tone] = STATUS[change.status];
  const lines = change.unified.split("\n");
  if (lines.at(-1) === "") lines.pop();
  const shown = lines.slice(0, MAX_DIFF_LINES);
  return h(
    "details",
    { class: "file", "data-file": change.path, open },
    h(
      "summary",
      {},
      h("span", { class: "mono path" }, change.path),
      chip(status, tone),
      h("span", { class: "adds", "aria-label": `${change.added} added` }, `+${change.added}`),
      h("span", { class: "dels", "aria-label": `${change.removed} removed` }, `−${change.removed}`),
    ),
    h(
      "div",
      { class: "diff", role: "group", "aria-label": `Changes to ${change.path}` },
      ...shown.map((line) => h("div", { class: `ln ${lineKind(line)}` }, line || " ")),
      lines.length > shown.length ? h("div", { class: "ln hunk" }, `… ${plural(lines.length - shown.length, "more line")}`) : null,
    ),
  );
}

function lineKind(line: string): "hunk" | "add" | "del" | "ctx" {
  if (line.startsWith("@@")) return "hunk";
  if (line.startsWith("+")) return "add";
  if (line.startsWith("-")) return "del";
  return "ctx";
}

/** "a", "a and b", "a, b and c". */
function sentence(words: string[]): string {
  if (words.length < 2) return words.join("");
  return `${words.slice(0, -1).join(", ")} and ${words.at(-1)}`;
}

function lower(text: string): string {
  return text.charAt(0).toLowerCase() + text.slice(1);
}

/** Marks a card for the end-to-end suite. */
function tagged(element: HTMLElement, test: string): HTMLElement {
  element.dataset.test = test;
  return element;
}
