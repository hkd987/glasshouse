// Remote help (CF-81, ADR 0028): the support tiers, which directory groups
// hold each, its tools, whose computers it may help with and for how long.
// Roles are policy: saved to the draft, previewed, published to the pilot
// ring. Support staff's access follows the pilot ring's policy, so a change
// takes effect once it is published.

import { h, show } from "@glasshouse/sdk";

import { type Impact, type RolesPage, type SupportRole, type SupportTool, api } from "./api.ts";
import { card, chip, duration, mono, notice, plural } from "./ui.ts";

/** What policy carries of a role: the page's extras left out. */
function plain(r: SupportRole): SupportRole {
  return {
    id: r.id,
    name: r.name,
    groups: [...r.groups],
    tools: [...r.tools],
    people: [...r.people],
    unattended: [...r.unattended],
    max_minutes: r.max_minutes,
  };
}

function words(text: string): string[] {
  return text
    .split(/[,\s]+/)
    .map((w) => w.trim())
    .filter(Boolean);
}

export async function rolesPage(root: HTMLElement): Promise<void> {
  let page = await api<RolesPage>("GET", "/api/roles");
  let forms = page.roles.map(plain);
  let impact: Impact | null = null;
  let message: { text: string; tone: "ok" | "bad" } | null = null;
  const fresh = { id: "", name: "" };

  async function act(work: () => Promise<string | null>): Promise<void> {
    try {
      const text = await work();
      message = text ? { text, tone: "ok" } : null;
    } catch (error) {
      message = { text: (error as Error).message, tone: "bad" };
    }
    page = await api<RolesPage>("GET", "/api/roles");
    forms = page.roles.map(plain);
    draw();
  }

  async function save(id: string, role: SupportRole | null): Promise<void> {
    await api("POST", "/api/policy/edit", { edit: "support_role", id, role });
    impact = await api<Impact>("POST", "/api/policy/preview", {});
  }

  function checks(name: string, options: { id: string; title: string }[], chosen: () => string[], set: (next: string[]) => void): HTMLElement {
    return h(
      "div",
      { class: "chips" },
      ...options.map((o) =>
        h(
          "label",
          { class: "check-row inline" },
          h("input", {
            type: "checkbox",
            name: `${name}-${o.id}`,
            checked: chosen().includes(o.id),
            onchange: (e: Event) => {
              const on = (e.target as HTMLInputElement).checked;
              const rest = chosen().filter((x) => x !== o.id);
              set(on ? [...rest, o.id] : rest);
            },
          }),
          h("span", {}, o.title),
        ),
      ),
    );
  }

  function roleCard(role: SupportRole, shown: RolesPage["roles"][number] | undefined): HTMLElement {
    const groups = page.groups.filter((g) => g.name !== "everyone").map((g) => ({ id: g.name, title: g.display }));
    const people = page.groups.map((g) => ({ id: g.name, title: g.display }));
    const holders = shown
      ? `${shown.holders.join(", ")}${shown.holder_count > shown.holders.length ? ` and ${plural(shown.holder_count - shown.holders.length, "other")}` : ""}`
      : "";
    return h(
      "section",
      { class: "card", "data-role": role.id },
      h("h2", {}, role.name, " ", mono(role.id)),
      h("p", { class: "card-sub" }, shown?.holder_count ? `Held by ${holders}.` : "Nobody holds it yet: no group it names has members."),
      h(
        "label",
        { class: "field" },
        "Name",
        h("input", { class: "text", name: "name", value: role.name, oninput: (e: Event) => (role.name = (e.target as HTMLInputElement).value) }),
      ),
      h("div", { class: "field" }, h("span", {}, "Held by these directory groups"), checks("group", groups, () => role.groups, (next) => (role.groups = next))),
      h(
        "div",
        { class: "field" },
        h("span", {}, "Tools"),
        checks(
          "tool",
          page.tools.map((t) => ({ id: t.id, title: t.title })),
          () => role.tools,
          (next) => (role.tools = next as SupportTool[]),
        ),
      ),
      h("div", { class: "field" }, h("span", {}, "Helps people in"), checks("people", people, () => role.people, (next) => (role.people = next))),
      h(
        "div",
        { class: "pair" },
        h(
          "label",
          { class: "field" },
          "Longest session, minutes",
          h("input", {
            class: "text",
            name: "max_minutes",
            inputmode: "numeric",
            value: String(role.max_minutes),
            oninput: (e: Event) => (role.max_minutes = Number((e.target as HTMLInputElement).value) || 0),
          }),
        ),
        h(
          "label",
          { class: "field" },
          "Device types it reaches with nobody there",
          h("input", {
            class: "text",
            name: "unattended",
            value: role.unattended.join(", "),
            placeholder: "None",
            oninput: (e: Event) => (role.unattended = words((e.target as HTMLInputElement).value)),
          }),
        ),
      ),
      h(
        "div",
        { class: "actions" },
        h(
          "button",
          {
            class: "button primary",
            "data-test": "save-role",
            onclick: () =>
              act(async () => {
                await save(role.id, role);
                return "Saved to the draft. Preview, then publish to the pilot ring.";
              }),
          },
          "Save to draft",
        ),
        h(
          "button",
          {
            class: "button",
            onclick: () =>
              act(async () => {
                await save(role.id, null);
                return `${role.name} taken out of the draft.`;
              }),
          },
          "Remove",
        ),
      ),
    );
  }

  function draw(): void {
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
            h("div", { class: "crumbs" }, "Remote help"),
            h("h1", {}, "Remote help"),
            h(
              "p",
              { class: "sub" },
              "Who may help people at their computers, and with what. Being an IT admin gives none of these tools: only a role naming one of your groups does.",
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
                      note: impact?.changes.find((c) => c.startsWith("Remote help")) ?? "Remote help roles",
                    });
                    impact = null;
                    return `Published v${r.version} to the pilot ring. Support staff's access follows it now.`;
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
            ...forms.map((role) =>
              roleCard(
                role,
                page.roles.find((r) => r.id === role.id),
              ),
            ),
            card(
              "Add a role",
              "It starts with viewing the screen; give it groups and tools, then save.",
              h(
                "div",
                { class: "server-row" },
                h("label", { class: "field" }, "Id", h("input", { class: "text", name: "new-id", placeholder: "tier-2", oninput: (e: Event) => (fresh.id = (e.target as HTMLInputElement).value) })),
                h("label", { class: "field" }, "Name", h("input", { class: "text", name: "new-name", placeholder: "Tier 2 support", oninput: (e: Event) => (fresh.name = (e.target as HTMLInputElement).value) })),
              ),
              h(
                "div",
                { class: "actions" },
                h(
                  "button",
                  {
                    class: "button",
                    onclick: () => {
                      if (!fresh.id.trim() || forms.some((r) => r.id === fresh.id.trim())) {
                        message = { text: "Give the role an id no other role has.", tone: "bad" };
                        draw();
                        return;
                      }
                      forms.push({ id: fresh.id.trim(), name: fresh.name.trim() || fresh.id.trim(), groups: [], tools: ["view"], people: ["everyone"], unattended: [], max_minutes: 60 });
                      fresh.id = "";
                      fresh.name = "";
                      message = null;
                      draw();
                    },
                  },
                  "Add",
                ),
              ),
            ),
          ),
          h(
            "aside",
            { class: "side" },
            card(
              "Versions and rollout",
              "Support staff sign in with the roles the pilot ring's policy names.",
              h("div", { class: "kv" }, h("span", {}, "Pilot ring"), mono(page.pilot_version ? `v${page.pilot_version}` : "–")),
              h("div", { class: "kv" }, h("span", {}, "Draft"), page.changed ? chip("Changed", "wait") : chip("Same as pilot", "ok")),
              h("div", { class: "kv" }, h("span", {}, "Time to answer a request"), h("span", {}, duration(page.answer_within_secs))),
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
