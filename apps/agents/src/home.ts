// Home: the person's agents, like a team chat (ADR 0065 §12). Questions
// waiting on them first, answerable right there; then each agent with
// what it's doing and your computer's state. Plain words only.

import { h, show } from "@glasshouse/sdk";

import type { AgentView, PendingApproval } from "./api.ts";
import { answer, app } from "./app.ts";
import { computerWords, summary, tabs, visible, whenLine } from "./model.ts";
import { button, icon, keepFocus, statusChip, thumbnail } from "./ui.ts";

export function drawHome(main: HTMLElement): void {
  keepFocus(main, () =>
    show(
      main,
      h(
        "div",
        { class: "page home" },
        h(
          "div",
          { class: "home-head" },
          h("div", {}, h("h1", {}, "Your agents"), h("p", { class: "sub" }, summary(app.agents))),
          h("a", { class: "btn primary big", href: "/new", "data-key": "new" }, icon("plus"), "New agent"),
        ),
        app.approvals.length ? waiting(app.approvals) : null,
        app.agents.length ? filters() : null,
        cards(),
      ),
    ),
  );
}

function waiting(approvals: PendingApproval[]): HTMLElement {
  return h(
    "section",
    { class: "waiting", id: "waiting", "aria-labelledby": "waiting-title" },
    h("h2", { id: "waiting-title" }, "Waiting on you"),
    h("ul", {}, ...approvals.map(question)),
  );
}

/** One question. A confidential one says only who needs them: its words
 * are for the conversation, opened on purpose. */
function question(p: PendingApproval): HTMLElement {
  const a = p.approval;
  const open = h("a", { class: "btn secondary", href: `/agents/${p.agent}?approval=${encodeURIComponent(a.id)}`, "data-key": `open-${a.id}`, "aria-label": `Open ${p.agent_name}` }, "Open");
  if (a.confidential) {
    return h(
      "li",
      { class: "question confidential" },
      h("span", { class: "who" }, p.agent_name),
      h("span", { class: "what" }, h("span", {}, icon("lock", "inline"), "Needs your answer on something confidential. Open it to see.")),
      h("span", { class: "row" }, open),
    );
  }
  let busy = false;
  const answerWith = (yes: boolean) => async () => {
    if (busy) return;
    busy = true;
    await answer(p.agent, p.agent_name, a, yes);
    busy = false;
  };
  return h(
    "li",
    { class: "question" },
    h("span", { class: "who" }, p.agent_name),
    h("span", { class: "what" }, h("span", {}, a.summary), a.detail ? h("span", { class: "detail" }, a.detail) : null),
    h(
      "span",
      { class: "row" },
      button(a.yes_label, answerWith(true), "primary", { "data-key": `yes-${a.id}` }),
      button("Don’t", answerWith(false), "secondary", { "data-key": `no-${a.id}`, "aria-label": `Don’t: ${a.yes_label}` }),
      open,
    ),
  );
}

function filters(): HTMLElement {
  return h(
    "div",
    { class: "tabs", role: "group", "aria-label": "Show agents" },
    ...tabs(app.agents, app.tab).map(({ tab, label, count }) =>
      button(
        [label, h("span", { class: "count" }, String(count))],
        () => {
          app.tab = tab;
          app.redraw();
        },
        `tab${app.tab === tab ? " on" : ""}`,
        { "aria-pressed": String(app.tab === tab), "data-key": `tab-${tab}` },
      ),
    ),
  );
}

function cards(): HTMLElement {
  if (app.agents.length === 0) {
    return h(
      "div",
      { class: "empty-card" },
      h("p", {}, "You don’t have any agents yet. They all work on a computer of your own, and ask you before anything leaves it."),
      h("a", { class: "btn primary", href: "/new" }, "Add your first agent"),
    );
  }
  const shown = visible(app.agents, app.tab);
  if (shown.length === 0) return h("p", { class: "empty-card" }, "Nothing here right now.");
  const now = new Date();
  return h("ul", { class: "cards", "aria-label": "Agents" }, ...shown.map((a) => h("li", {}, card(a, now))));
}

function card(a: AgentView, now: Date): HTMLElement {
  const words = computerWords(a.computer);
  const live = a.computer.state === "running" && (a.status === "working" || a.status === "taken_over");
  return h(
    "a",
    { class: `card status-${a.status}`, href: `/agents/${a.id}`, "data-key": `card-${a.id}` },
    thumbnail(words.short, live, a.computer.state === "asleep"),
    h(
      "span",
      { class: "card-body" },
      h("span", { class: "card-top" }, h("span", { class: "names" }, h("span", { class: "name" }, a.name), h("span", { class: "role" }, a.role)), statusChip(a.status)),
      h("span", { class: "doing" }, a.doing),
      h(
        "span",
        { class: "when" },
        whenLine(a, now),
        a.pending > 0 ? h("span", { class: "pending" }, ` · ${a.pending === 1 ? "1 question" : `${a.pending} questions`} for you`) : null,
      ),
      h("span", { class: "visually-hidden" }, `Its computer: ${words.short}.`),
    ),
  );
}
