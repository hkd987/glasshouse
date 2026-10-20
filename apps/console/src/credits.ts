// Model credits (ADR 0039): model usage the org buys from Glasshouse, for
// orgs without Amazon Bedrock or Claude Code. Each computer calls models
// itself with the org's key, through OpenRouter, which picks the best model
// for each request; the key's limit is what the org bought, so no computer
// can spend more. Turning credits on for agents is a policy edit, published
// like any rule. The key itself never comes to this page.

import { h, show } from "@glasshouse/sdk";

import { type CreditsPage, type Impact, api } from "./api.ts";
import { card, chip, notice, stat } from "./ui.ts";

const dollars = (n: number | null): string => (n === null ? "–" : `$${n.toFixed(2)}`);

export async function creditsPage(root: HTMLElement): Promise<void> {
  let page = await api<CreditsPage>("GET", "/api/credits");
  let message: { text: string; tone: "ok" | "bad" } | null = null;

  async function act(work: () => Promise<string | null>): Promise<void> {
    try {
      const text = await work();
      message = text ? { text, tone: "ok" } : null;
    } catch (error) {
      message = { text: (error as Error).message, tone: "bad" };
    }
    page = await api<CreditsPage>("GET", "/api/credits");
    draw();
  }

  function use(on: boolean): Promise<void> {
    return act(async () => {
      await api("POST", "/api/policy/edit", { edit: "credits", on });
      await api<Impact>("POST", "/api/policy/preview", {});
      return on
        ? "Saved to the draft: agents allowed to call models use credits once you publish it from Policies."
        : "Saved to the draft: agents stop using credits once you publish it from Policies.";
    });
  }

  function draw(): void {
    show(
      root,
      h(
        "div",
        { class: "page", "data-test": "credits" },
        h(
          "header",
          { class: "page-head" },
          h(
            "div",
            {},
            h("div", { class: "crumbs" }, "Model credits"),
            h("h1", {}, "Model credits"),
            h(
              "p",
              { class: "sub" },
              "Model usage your org buys from Glasshouse, for agents when you don't use Amazon Bedrock or your own key. Each request goes to the model best suited to it.",
            ),
          ),
          page.offered
            ? h(
                "div",
                { class: "actions" },
                h(
                  "button",
                  { class: page.on ? "button" : "button primary", "data-test": "use-credits", onclick: () => use(!page.on) },
                  page.on ? "Stop using credits" : "Use credits for agents",
                ),
              )
            : null,
        ),
        message ? notice(message.text, message.tone) : null,
        page.problem ? notice(`Usage couldn't be read just now: ${page.problem}`, "bad") : null,
        !page.offered
          ? card("Not offered here", "This console doesn't sell model credits.")
          : h(
              "div",
              {},
              h(
                "div",
                { class: "stats" },
                card(null, null, stat("Bought", dollars(page.granted_usd))),
                card(null, null, stat("Spent", dollars(page.spent_usd), "blue")),
                card(null, null, stat("Left", dollars(page.remaining_usd), page.remaining_usd !== null && page.remaining_usd < page.granted_usd * 0.1 ? "orange" : null)),
              ),
              h(
                "div",
                { class: "columns" },
                h(
                  "div",
                  { class: "main" },
                  card(
                    "For agents",
                    null,
                    h("p", {}, page.on ? chip("On in the draft", "ok") : chip("Off", "plain")),
                    h(
                      "p",
                      { class: "card-sub" },
                      "Public and internal data only, unless you add credits to the confidential route in Policies. Agents also need to be allowed to call models.",
                    ),
                  ),
                  card(
                    "Purchases",
                    page.grants.length ? null : "None yet.",
                    ...page.grants.map((g) =>
                      h("div", { class: "grant", "data-test": "grant" }, h("span", {}, new Date(g.at).toLocaleDateString()), h("span", {}, dollars(g.usd)), h("span", { class: "card-sub" }, g.note)),
                    ),
                  ),
                ),
                h(
                  "div",
                  { class: "side" },
                  card(
                    "Buying more",
                    "Glasshouse adds credits to your org for now: ask your account contact. Paying by card comes later. Unused credits expire a year after purchase.",
                  ),
                  card(
                    "Where requests go",
                    "Each computer sends its own requests through OpenRouter, only to providers that keep no data and don't train on it. Prompts never pass through Glasshouse's servers, and each request's model and cost are in the computer's audit log.",
                  ),
                ),
              ),
            ),
      ),
    );
  }

  draw();
}
