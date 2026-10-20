// Model credits (ADR 0039): model usage the org buys from Glasshouse, for
// orgs without Amazon Bedrock or Claude Code, paid by card on Stripe's own
// page. Each computer calls models itself with the org's key, through
// OpenRouter, which picks the best model for each request; the key's limit
// is what the org bought, so no computer can spend more. Turning credits on
// for agents is a policy edit, published like any rule, once an admin
// accepted the model providers' terms. The key itself never comes here.

import { h, show } from "@glasshouse/sdk";

import { type CreditsPage, type Impact, api } from "./api.ts";
import { card, chip, notice, stat } from "./ui.ts";

const dollars = (n: number | null): string => (n === null ? "–" : `$${n.toFixed(2)}`);

/** Where Stripe sends the admin back to: `paid` or `cancelled`. */
const RETURNS: Record<string, { text: string; tone: "ok" | "bad" }> = {
  paid: { text: "Payment received. Your credits show here as soon as Stripe confirms it, usually within a minute.", tone: "ok" },
  cancelled: { text: "Payment cancelled: nothing was charged.", tone: "bad" },
};

export async function creditsPage(root: HTMLElement, returned?: string): Promise<void> {
  let page = await api<CreditsPage>("GET", "/api/credits");
  let message: { text: string; tone: "ok" | "bad" } | null = returned ? (RETURNS[returned] ?? null) : null;
  let amount = 100;
  let accepting = false;

  /** Back from paying: looks again, in the background, until Stripe's confirmation lands. */
  async function watch(): Promise<void> {
    const before = page.granted_usd;
    for (let i = 0; i < 20; i++) {
      await new Promise((r) => setTimeout(r, 1500));
      if (!document.contains(root)) return;
      page = await api<CreditsPage>("GET", "/api/credits");
      if (page.granted_usd !== before) {
        message = { text: "Your payment is in: the credits are ready.", tone: "ok" };
        draw();
        return;
      }
    }
  }

  function buy(): Promise<void> {
    return act(async () => {
      const r = await api<{ url: string }>("POST", "/api/credits/checkout", { usd: amount });
      window.location.assign(r.url);
      return null;
    });
  }

  function accept(): Promise<void> {
    return act(async () => {
      if (!accepting) throw new Error("Tick the box to accept the terms for your org.");
      await api("POST", "/api/credits/terms", { accept: true });
      return "Terms accepted for your org.";
    });
  }

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
                    page.terms_accepted
                      ? null
                      : h(
                          "div",
                          { "data-test": "terms" },
                          h(
                            "label",
                            { class: "check" },
                            h("input", { type: "checkbox", name: "terms", onchange: (e: Event) => (accepting = (e.target as HTMLInputElement).checked) }),
                            " For our org, I accept OpenRouter's terms and the terms of the model providers it routes to.",
                          ),
                          h("button", { class: "button", "data-test": "accept-terms", onclick: () => accept() }, "Accept"),
                        ),
                  ),
                  card(
                    "Purchases",
                    page.grants.length ? null : "None yet.",
                    ...page.grants.map((g) =>
                      h(
                        "div",
                        { class: "grant", "data-test": "grant" },
                        h("span", {}, new Date(g.at).toLocaleDateString()),
                        h("span", {}, dollars(g.usd)),
                        h("span", { class: "card-sub" }, g.note || (g.paid ? "Paid by card" : "Added by Glasshouse")),
                      ),
                    ),
                  ),
                ),
                h(
                  "div",
                  { class: "side" },
                  page.can_buy
                    ? card(
                        "Buy credits",
                        "Paid by card on Stripe's page. Unused credits expire a year after purchase.",
                        h(
                          "label",
                          { class: "field" },
                          "Amount (US dollars)",
                          h("input", {
                            class: "text",
                            type: "number",
                            name: "usd",
                            min: String(page.min_usd),
                            max: String(page.max_usd),
                            step: "1",
                            value: String(amount),
                            oninput: (e: Event) => (amount = Number((e.target as HTMLInputElement).value)),
                          }),
                        ),
                        page.terms_accepted ? null : h("p", { class: "card-sub" }, "Accept the model providers' terms first, under For agents."),
                        h("button", { class: "button primary", "data-test": "buy", disabled: !page.terms_accepted, onclick: () => buy() }, "Pay with card"),
                      )
                    : card("Buying more", "Glasshouse adds credits to your org: ask your account contact. Unused credits expire a year after purchase."),
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
  if (returned === "paid") void watch();
}
