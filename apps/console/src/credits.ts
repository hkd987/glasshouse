// Model credits (ADR 0039): model usage the org buys from Glasshouse, for
// orgs without Amazon Bedrock or Claude Code, paid by card on Stripe's own
// page. Each computer sends its calls to this console, signed with its own
// key; the console forwards them to OpenRouter, which picks the best model
// for each request, with the org's key, capped at what the org bought.
// Turning credits on for agents is a policy edit, published like any rule,
// once an admin accepted the model providers' terms. The key never leaves
// our servers, and never comes to this page.

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
  let alertBelow: number | null = null;

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

  function setAlert(usd: number | null): Promise<void> {
    return act(async () => {
      const r = await api<{ alert_below_usd: number }>("POST", "/api/credits/alert", { usd });
      return `You'll be warned below ${dollars(r.alert_below_usd)} left.`;
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
        page.standing === "used_up"
          ? h("div", { "data-test": "credits-standing" }, notice("Your credits are used up. Agents' calls on credits go to the next model your policy routes to, or don't go at all, until you buy more.", "bad"))
          : page.standing === "low"
            ? h(
                "div",
                { "data-test": "credits-standing" },
                notice(`Your credits are running low: ${dollars(page.remaining_usd)} left. Agents stop using them when they run out.`, "bad"),
              )
            : null,
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
                card(null, null, stat("Left", dollars(page.remaining_usd), page.standing ? "orange" : null)),
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
                    h(
                      "p",
                      {},
                      page.on && page.live
                        ? chip("On", "ok")
                        : page.on
                          ? chip("On in the draft: publish it in Policies", "ok")
                          : page.live
                            ? chip("Off in the draft: on until it's published", "plain")
                            : chip("Off", "plain"),
                    ),
                    h(
                      "p",
                      { class: "card-sub" },
                      "Public and internal data only, unless you add credits to the confidential route in Policies. Choose the agents that ask AI models in Policies, under Data labels and models.",
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
                    "Low balance",
                    `Admins see a warning here and beside Model credits on every page below ${dollars(page.alert_below_usd)} left${page.alert_set ? "" : " (a tenth of what you bought, at least $5)"}.`,
                    h(
                      "div",
                      { "data-test": "alert" },
                      h(
                        "label",
                        { class: "field" },
                        "Warn below (US dollars)",
                        h("input", {
                          class: "text",
                          type: "number",
                          name: "alert",
                          min: "0",
                          step: "1",
                          value: String(page.alert_below_usd),
                          oninput: (e: Event) => (alertBelow = Number((e.target as HTMLInputElement).value)),
                        }),
                      ),
                      h("button", { class: "button", "data-test": "save-alert", onclick: () => setAlert(alertBelow ?? page.alert_below_usd) }, "Save"),
                      page.alert_set ? h("button", { class: "button", onclick: () => setAlert(null) }, "Use the default") : null,
                    ),
                  ),
                  card(
                    "By computer",
                    page.by_computer.length ? "What each computer's agents spent. What they asked is never kept." : "No computer has used credits yet.",
                    ...page.by_computer.map((c) =>
                      h(
                        "div",
                        { class: "grant", "data-test": "computer" },
                        h("span", { class: "mono" }, c.device),
                        h("span", {}, c.calls && c.spent_usd < 0.01 ? "under $0.01" : dollars(c.spent_usd)),
                        h("span", { class: "card-sub" }, `${c.calls} ${c.calls === 1 ? "call" : "calls"}`),
                      ),
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
                    "Each computer sends its requests to Glasshouse, signed with its own key, already stripped of secrets and personal details. Glasshouse passes them to OpenRouter, only to providers that keep no data and don't train on it, and keeps only which computer called, the tokens and the cost. Your org's OpenRouter key never leaves our servers.",
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
