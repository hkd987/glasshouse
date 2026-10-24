// Agent policy for one group (AD-2, AD-3), after the "Admin · agent policy"
// mockup: approval tiers by action class, allowed agents, apps in people's
// profiles, data labels and models; preview impact, publish to the pilot
// ring, promote, roll back; and the group's live numbers (AD-4).

import { h, show } from "@glasshouse/sdk";

import { type Impact, type PolicyPage, type Ring, type Tier, api } from "./api.ts";
import { capital, card, chip, mono, notice, plural, stat } from "./ui.ts";

const TIERS: Tier[] = ["auto", "notify", "ask", "never"];
/** The action an agent needs to ask a model (ADR 0018). */
const MODEL_COMPLETE = "model.complete";
/** Apps a profile may hold, as policy names them. */
const PROFILE_APPS: [string, string][] = [
  ["browser", "Browser"],
  ["mail", "Mail"],
  ["calendar", "Calendar"],
  ["store", "Store"],
  ["console", "Admin console"],
  ["activity-monitor", "Activity Monitor"],
  ["terminal", "Terminal"],
];

interface View {
  page: PolicyPage;
  impact: Impact | null;
  message: { text: string; tone: "ok" | "bad" } | null;
  busy: boolean;
}

export async function policyPage(root: HTMLElement, group: string | null): Promise<void> {
  const view: View = { page: await load(group), impact: null, message: null, busy: false };
  const draw = () => show(root, render(view, act));
  async function act(work: () => Promise<string | null>, keepImpact = false): Promise<void> {
    if (view.busy) return;
    view.busy = true;
    // Until this lands, a click would be dropped: say so instead of losing it.
    for (const button of root.querySelectorAll<HTMLButtonElement>("button")) button.disabled = true;
    try {
      const text = await work();
      view.message = text ? { text, tone: "ok" } : null;
    } catch (error) {
      view.message = { text: (error as Error).message, tone: "bad" };
    }
    view.page = await load(view.page.group.name);
    if (!keepImpact) view.impact = null;
    view.busy = false;
    draw();
  }
  draw();
}

function load(group: string | null): Promise<PolicyPage> {
  return api<PolicyPage>("GET", `/api/policy${group ? `?group=${encodeURIComponent(group)}` : ""}`);
}

type Act = (work: () => Promise<string | null>, keepImpact?: boolean) => Promise<void>;

function edit(body: Record<string, unknown>): Promise<string | null> {
  return api("POST", "/api/policy/edit", body).then(() => null);
}

function render(view: View, act: Act): HTMLElement {
  const p = view.page;
  const g = p.group;
  const source = g.source ? `synced from the ${g.source}` : "every person in the org";
  const header = h(
    "header",
    { class: "page-head" },
    h(
      "div",
      {},
      h(
        "div",
        { class: "crumbs" },
        "Policies / ",
        h(
          "select",
          {
            class: "group-pick",
            "aria-label": "Group",
            onchange: (e: Event) => {
              location.hash = `#/policy/${(e.target as HTMLSelectElement).value}`;
            },
          },
          ...p.groups.map((x) => h("option", { value: x.name, selected: x.name === g.name }, x.display)),
        ),
      ),
      h("h1", {}, `${g.display} · agent policy`),
      h(
        "p",
        { class: "sub" },
        `Applies to ${plural(g.users, "user")} · ${source} · `,
        mono(p.draft_changed ? `draft v${p.draft_version}` : `v${p.rings.pilot ?? "–"} in force`),
      ),
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
              view.impact = await api<Impact>("POST", "/api/policy/preview", {});
              return null;
            }, true),
        },
        "Preview impact",
      ),
      h(
        "button",
        {
          class: "button primary",
          "data-test": "publish",
          // What goes out is what IT previewed (AD-3).
          disabled: !p.draft_changed || !view.impact,
          title: view.impact ? undefined : "Preview the impact first",
          onclick: () =>
            act(async () => {
              const r = await api<{ version: number }>("POST", "/api/policy/publish", {
                note: view.impact?.changes[0] ?? `${g.display}: policy changed`,
                previewed: view.impact?.draft,
              });
              return `Published v${r.version} to the pilot ring. Pilot devices apply it in about a second.`;
            }),
        },
        "Publish to pilot ring",
      ),
    ),
  );

  const tiers = card(
    "Approval tiers",
    "Users can make these stricter for their own tasks, never looser.",
    h(
      "table",
      { class: "tiers" },
      h("thead", {}, h("tr", {}, h("th", {}, "Action"), h("th", {}, "Tier"))),
      h(
        "tbody",
        {},
        ...p.classes.map((c) =>
          h(
            "tr",
            { "data-class": c.id },
            h(
              "td",
              {},
              h("div", {}, c.label),
              c.inherited ? h("div", { class: "hint" }, "From the org's rules") : null,
            ),
            h(
              "td",
              {},
              h(
                "div",
                { class: "segmented", role: "radiogroup", "aria-label": c.label },
                ...TIERS.map((t) =>
                  h(
                    "button",
                    {
                      class: `seg ${t}${c.tier === t ? " on" : ""}`,
                      role: "radio",
                      "aria-checked": c.tier === t ? "true" : "false",
                      onclick: () => act(() => edit({ edit: "tier", group: g.name, class: c.id, tier: t })),
                    },
                    capital(t),
                  ),
                ),
              ),
            ),
          ),
        ),
      ),
    ),
  );

  const agents = card(
    "Allowed agents",
    null,
    ...p.agents.map((a) =>
      h(
        "label",
        { class: "check-row" },
        h(
          "span",
          {},
          h("div", {}, a.name),
          mono(
            [a.version ? `v${a.version}` : null, a.everyone && g.name !== "everyone" ? "everyone may run it" : null, a.egress.length ? `network ${a.egress.join(", ")}` : "no network"]
              .filter(Boolean)
              .join(" · "),
          ),
        ),
        h("input", {
          type: "checkbox",
          checked: a.allowed,
          onchange: (e: Event) =>
            act(() => edit({ edit: "agent", group: g.name, agent: a.id, allowed: (e.target as HTMLInputElement).checked })),
        }),
      ),
    ),
  );

  const labels = card(
    "Data labels and models",
    null,
    ...p.routes.map((r) =>
      h(
        "div",
        { class: "kv" },
        h("span", {}, capital(r.label)),
        h("span", { class: r.route === "Blocked" ? "strong" : "" }, r.route),
      ),
    ),
    h(
      "label",
      { class: "kv" },
      h("span", {}, "Most sensitive data their agents open"),
      h(
        "select",
        {
          onchange: (e: Event) => {
            const value = (e.target as HTMLSelectElement).value;
            return act(() => edit({ edit: "max_label", group: g.name, label: value || null }));
          },
        },
        h("option", { value: "", selected: !p.max_label }, `The org's (${p.org_max_label})`),
        ...["public", "internal", "confidential"].map((l) => h("option", { value: l, selected: p.max_label === l }, capital(l))),
      ),
    ),
    // AD-2, ADR 0039: which agents may ask a model at all; the routes above
    // say which models, for each kind of data.
    h("div", { class: "hint" }, "Agents that ask AI models, in every group"),
    ...p.agents.map((a) =>
      h(
        "label",
        { class: "check-row", "data-test": `model-calls-${a.id}` },
        h("span", {}, a.name),
        h("input", {
          type: "checkbox",
          checked: a.actions.includes(MODEL_COMPLETE),
          onchange: (e: Event) => act(() => edit({ edit: "model_calls", agent: a.id, on: (e.target as HTMLInputElement).checked })),
        }),
      ),
    ),
  );

  const profiles = card(
    "Apps in their profiles",
    "Which apps people get is policy: the device offers and opens only these.",
    ...PROFILE_APPS.map(([id, name]) =>
      h(
        "label",
        { class: "check-row" },
        h("span", {}, name),
        h("input", {
          type: "checkbox",
          checked: p.apps.includes(id),
          onchange: (e: Event) => act(() => edit({ edit: "app", group: g.name, app: id, on: (e.target as HTMLInputElement).checked })),
        }),
      ),
    ),
  );

  const rollout = card(
    "Versions and rollout",
    "A version reaches the pilot ring first, then early, then broad. Rolling back is a new version, everywhere at once.",
    h(
      "div",
      { class: "rings" },
      ...(["pilot", "early", "broad"] as Ring[]).map((r) => h("div", { class: "ring" }, h("span", { class: "hint" }, capital(r)), mono(`v${p.rings[r] ?? "–"}`))),
      h(
        "button",
        {
          class: "button",
          disabled: p.rings.pilot === p.rings.early,
          onclick: () => act(async () => `Early ring now runs v${(await api<{ version: number }>("POST", "/api/policy/promote", { ring: "early", version: p.rings.pilot })).version}.`),
        },
        "Promote to early",
      ),
      h(
        "button",
        {
          class: "button",
          disabled: p.rings.early === p.rings.broad,
          onclick: () => act(async () => `Every device now runs v${(await api<{ version: number }>("POST", "/api/policy/promote", { ring: "broad", version: p.rings.early })).version}.`),
        },
        "Promote to broad",
      ),
    ),
    h(
      "table",
      { class: "versions" },
      h(
        "tbody",
        {},
        ...p.versions.map((v) =>
          h(
            "tr",
            {},
            h("td", {}, mono(`v${v.version}`)),
            h("td", {}, v.note || "No note", h("div", { class: "hint" }, `${v.by} · ${new Date(v.at).toLocaleString("en-US")}`)),
            h("td", {}, ...v.rings.map((r) => chip(r, "ok"))),
            h(
              "td",
              {},
              v.version !== p.rings.pilot
                ? h(
                    "button",
                    {
                      class: "button small",
                      onclick: () =>
                        act(async () => {
                          const r = await api<{ version: number; discarded_draft: boolean }>("POST", "/api/policy/rollback", { to: v.version });
                          const draft = r.discarded_draft ? " The draft's changes that weren't published were discarded." : "";
                          return `Rolled back to v${v.version}'s rules as v${r.version}, on every device.${draft}`;
                        }),
                    },
                    `Roll back to v${v.version}`,
                  )
                : null,
            ),
          ),
        ),
      ),
    ),
  );

  const live = h(
    "section",
    { class: "card dark live" },
    h("h2", {}, `Live · ${p.live.group}`),
    stat("Agents running", p.live.running, "blue"),
    stat("Waiting on a person", p.live.waiting, "orange"),
    stat("Blocked by policy, 24h", p.live.blocked_24h),
    stat("Rollbacks, 7 days", p.live.rollbacks_7d),
  );

  const impact = view.impact
    ? card(
        "Preview impact",
        view.impact.base_version ? `Compared with v${view.impact.base_version}, which every device runs.` : null,
        h(
          "p",
          { class: "impact", "data-test": "impact" },
          view.impact.changes.length
            ? `This change affects ${plural(view.impact.users, "user")} and ${plural(view.impact.agents, "agent")}.`
            : "Nothing changes: the draft is what every device runs.",
        ),
        view.impact.changes.length ? h("ul", { class: "changes" }, ...view.impact.changes.map((c) => h("li", {}, c))) : null,
        ...view.impact.warnings.map((w) => h("p", { class: "warning" }, w)),
        p.draft_changed
          ? h(
              "button",
              {
                class: "button small",
                onclick: () =>
                  act(async () => {
                    await api("POST", "/api/policy/discard", {});
                    return "Draft discarded.";
                  }),
              },
              "Discard draft",
            )
          : null,
      )
    : null;

  return h(
    "div",
    { class: "page" },
    header,
    view.message ? notice(view.message.text, view.message.tone) : null,
    h(
      "div",
      { class: "columns" },
      h("div", { class: "main" }, tiers, h("div", { class: "pair" }, agents, labels), h("div", { class: "pair" }, profiles, rollout)),
      h("aside", { class: "side" }, live, impact),
    ),
  );
}
