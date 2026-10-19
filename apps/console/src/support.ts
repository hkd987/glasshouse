// Support (CF-80, CF-81, ADR 0028): what a support person's roles let
// them do, which role has the tools theirs don't, and the computers their
// roles reach. Asking to see a screen comes with sessions (slice b), so
// every tool says it isn't available yet rather than pretending.

import { h, show } from "@glasshouse/sdk";

import { type SupportPage, api } from "./api.ts";
import { ago, card, chip, duration, mono, notice, plural } from "./ui.ts";

/** Rows the computer list shows before asking for a narrower search. */
const SHOWN = 100;

export async function supportPage(root: HTMLElement): Promise<void> {
  const page = await api<SupportPage>("GET", "/api/support");
  let search = "";

  function computers(): HTMLElement {
    const q = search.trim().toLowerCase();
    const found = q ? page.devices.filter((d) => [d.id, d.name, ...d.users].some((x) => x.toLowerCase().includes(q))) : page.devices;
    return h(
      "div",
      { "data-test": "computers" },
      h(
        "table",
        { class: "list devices" },
        h("thead", {}, h("tr", {}, ...["Computer", "People", "Last seen", ""].map((x) => h("th", {}, x)))),
        h(
          "tbody",
          {},
          ...found.slice(0, SHOWN).map((d) =>
            h(
              "tr",
              { "data-device": d.id },
              h(
                "td",
                {},
                h("span", { class: `dot ${d.online ? "green" : "grey"}` }),
                mono(d.name),
                d.kind ? h("span", { class: "hint" }, ` · ${d.kind}`) : null,
                d.unattended ? h("div", {}, chip("Nobody needs to answer", "wait")) : null,
              ),
              h("td", {}, d.users.join(", ") || "–"),
              h("td", { class: "hint" }, ago(d.last_seen)),
              h("td", { class: "num" }, h("button", { class: "button", disabled: true, title: "Remote sessions aren't available yet" }, "Ask to see screen")),
            ),
          ),
        ),
      ),
      found.length > SHOWN ? h("p", { class: "hint" }, `${plural(found.length - SHOWN, "more computer")}: search to narrow.`) : null,
      found.length === 0 ? h("p", { class: "hint" }, q ? "No computer or person matches." : "No computers have checked in yet.") : null,
    );
  }

  function draw(): void {
    let list = computers();
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
            h("div", { class: "crumbs" }, "Support"),
            h("h1", {}, "Remote help"),
            h(
              "p",
              { class: "sub" },
              `${page.roles.map((r) => r.name).join(", ")} · people have ${duration(page.answer_within_secs)} to answer a request · `,
              mono(page.policy_version ? `policy v${page.policy_version}` : "no policy yet"),
            ),
          ),
        ),
        notice("Remote sessions aren't available yet. This page shows what your role can do and the computers it reaches."),
        h(
          "div",
          { class: "columns" },
          h(
            "div",
            { class: "main" },
            card(
              "Computers",
              `${plural(page.devices.length, "computer")} your role reaches, most recently seen first.`,
              h(
                "label",
                { class: "field" },
                "Find a computer or person",
                h("input", {
                  class: "text",
                  type: "search",
                  name: "search",
                  value: search,
                  placeholder: "Name or computer",
                  oninput: (e: Event) => {
                    search = (e.target as HTMLInputElement).value;
                    const next = computers();
                    list.replaceWith(next);
                    list = next;
                  },
                }),
              ),
              list,
            ),
          ),
          h(
            "aside",
            { class: "side" },
            card(
              "Your tools",
              "From your role. The rest belong to other roles.",
              h(
                "div",
                { "data-test": "tools" },
                ...page.tools.map((t) =>
                  h(
                    "div",
                    { class: "kv", "data-tool": t.id },
                    h("span", {}, t.title),
                    t.mine
                      ? chip(t.ready ? "Yours" : "Yours · not available yet", "ok")
                      : chip(t.roles.length ? `Needs ${t.roles.join(" or ")}` : "No role has it"),
                  ),
                ),
              ),
            ),
            ...page.roles.map((r) =>
              card(
                r.name,
                null,
                h("div", { class: "kv" }, h("span", {}, "Helps"), h("span", {}, r.people.join(", "))),
                h("div", { class: "kv" }, h("span", {}, "Longest session"), h("span", {}, duration(r.max_minutes * 60))),
                h("div", { class: "kv" }, h("span", {}, "With nobody there"), h("span", {}, r.unattended.length ? r.unattended.join(", ") : "Never")),
              ),
            ),
          ),
        ),
      ),
    );
  }
  draw();
}
