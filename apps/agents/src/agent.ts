// An agent: its conversation, and beside it your computer it works on, what it can
// reach and its record (ADR 0065 §12). Ask in plain words; its answers,
// files and questions come back as cards. The skeleton is drawn once and
// its parts redrawn as things change, so the ask box keeps its words and
// focus while the agent works.

import { h, show } from "@glasshouse/sdk";

import { type AgentDetail, type AgentView, type ApprovalView, type Asked, type Message, Refused, agentPath, api } from "./api.ts";
import { answer, app, control, go, loadDetail, undoControl } from "./app.ts";
import { type Row, ago, answerLine, computerWords, groupMessages, size, sleepAction, splitPath, spoken, undoable } from "./model.ts";
import { announce, avatar, button, clock, focusHeading, icon, keepFocus, notice, reachList, statusChip, thumbnail } from "./ui.ts";

export class AgentScreen {
  readonly id: string;
  private readonly root: HTMLElement;
  private readonly head = h("div", { class: "convo-head" });
  private readonly banner = h("div", { class: "convo-banner" });
  private readonly list = h("ol", { class: "messages", "aria-label": "Conversation" });
  private readonly side = h("aside", { class: "side", "aria-label": "Its computer and what it can reach" });
  private readonly input = h("input", { id: "ask", type: "text", autocomplete: "off", maxlength: "4000", enterkeyhint: "send" });
  private readonly label = h("label", { for: "ask", class: "visually-hidden" }, "Ask it something");
  private readonly send = h("button", { type: "submit", class: "btn primary big" }, "Send");
  private readonly formError = h("div", { class: "form-error" });
  private seen: Set<string> | null = null;
  private focusAfter: string | null = null;

  constructor(main: HTMLElement, id: string) {
    this.id = id;
    const form = h("form", { class: "composer", onsubmit: (e: Event) => void this.ask(e) }, this.label, this.input, this.send);
    this.root = h(
      "div",
      { class: "page agent" },
      h("div", { class: "back-row" }, h("a", { class: "back", href: "/" }, icon("back"), "All agents")),
      h(
        "div",
        { class: "agent-layout" },
        h("section", { class: "convo", "aria-labelledby": "agent-name" }, this.head, this.banner, this.list, this.formError, form),
        this.side,
      ),
    );
    show(main, this.root);
    show(this.list, h("li", { class: "loading" }, "Loading the conversation…"));
  }

  /** Loads it the first time, at the question `approval` names if any
   * (Teams' Open); main.ts calls `draw` after each change. A confidential
   * question still waits to be shown. */
  async open(approval: string | null = null): Promise<void> {
    try {
      await loadDetail(this.id);
      this.draw();
      this.list.scrollTop = this.list.scrollHeight;
      const card = approval ? this.list.querySelector<HTMLElement>(`[data-approval="${CSS.escape(approval)}"]`) : null;
      if (card) {
        card.classList.add("target");
        card.scrollIntoView({ block: "center" });
        const first = card.querySelector<HTMLElement>("button");
        if (first) first.focus({ preventScroll: true });
        else {
          card.tabIndex = -1;
          card.focus({ preventScroll: true });
        }
      } else focusHeading(this.root);
    } catch (error) {
      const words = error instanceof Refused && error.status === 404 ? "There’s no agent here by that name. It may have been removed." : (error as Error).message;
      show(this.list, h("li", {}, notice(words, "bad")));
    }
  }

  draw(): void {
    const detail = app.details.get(this.id);
    if (!detail) return;
    const agent = detail.agent;
    document.title = `${agent.name} · Glasshouse Agents`;
    this.label.textContent = `Ask ${agent.name} something`;
    this.input.placeholder = `Ask ${agent.name} something`;
    show(
      this.head,
      h("div", { class: "who" }, avatar(agent.name), h("div", {}, h("h1", { id: "agent-name" }, agent.name), h("p", { class: "role" }, agent.role))),
      statusChip(agent.status, true),
      h("p", { class: "doing" }, agent.doing),
    );
    show(this.banner, agent.status === "taken_over" ? this.takenOver(agent) : null);
    this.drawMessages(detail);
    keepFocus(this.side, () => show(this.side, ...this.sidePanel(detail)));
  }

  destroy(): void {
    this.root.remove();
  }

  private takenOver(agent: AgentView): HTMLElement {
    return h(
      "div",
      { class: "banner-yours", role: "status" },
      h("span", {}, `You have ${agent.name}’s computer. ${agent.name} waits until you hand it back.`),
      h(
        "span",
        { class: "row" },
        h("a", { class: "btn secondary", href: `/agents/${agent.id}/computer` }, "Open your computer"),
        button("Hand back", () => control(agent.id, "hand-back"), "primary", { "data-key": "hand-back" }),
      ),
    );
  }

  private drawMessages(detail: AgentDetail): void {
    const list = this.list;
    const atBottom = list.scrollHeight - list.scrollTop - list.clientHeight < 80;
    // New messages from the agent are said once, after the first load.
    const fresh = this.seen ? detail.messages.filter((m) => !this.seen?.has(m.id)) : [];
    this.seen = new Set(detail.messages.map((m) => m.id));
    for (const m of fresh) if (m.from === "agent") announce(spoken(detail.agent.name, m));

    const target = undoable(detail.messages);
    const rows = groupMessages(detail.messages, new Date());
    keepFocus(list, () =>
      show(
        list,
        ...(rows.length ? rows.map((row) => this.row(row, detail.agent, target?.message ?? null, target?.since ?? "")) : [h("li", { class: "empty" }, `Ask ${detail.agent.name} something to start.`)]),
        detail.agent.status === "working" || detail.agent.status === "starting"
          ? h("li", { class: "working-now", "aria-hidden": "true" }, h("span", { class: "dots" }, h("span", {}), h("span", {}), h("span", {})), detail.agent.doing || "Working")
          : null,
      ),
    );
    if (this.focusAfter) {
      list.querySelector<HTMLElement>(`[data-key="${CSS.escape(this.focusAfter)}"]`)?.focus();
      this.focusAfter = null;
    }
    if (atBottom || fresh.length) list.scrollTop = list.scrollHeight;
  }

  private row(row: Row, agent: AgentView, undoMessage: string | null, undoSince: string): HTMLElement {
    if (row.kind === "day") return h("li", { class: "day" }, h("span", {}, row.label));
    const mine = row.from === "person";
    return h(
      "li",
      { class: `run ${mine ? "mine" : "theirs"}` },
      h("span", { class: "visually-hidden" }, mine ? "You:" : `${agent.name}:`),
      ...row.items.map((m) => this.message(m, agent, m.id === undoMessage ? undoSince : null)),
      h("span", { class: "time" }, clock(row.at)),
    );
  }

  private message(m: Message, agent: AgentView, undoSince: string | null): HTMLElement {
    const body = m.body;
    switch (body.kind) {
      case "text":
        return h(
          "div",
          { class: "item" },
          h("div", { class: `bubble ${m.from === "person" ? "mine" : "theirs"}` }, body.text),
          body.files?.length
            ? h(
                "ul",
                { class: "files", "aria-label": "Files copied into your computer" },
                ...body.files.map((f) => h("li", { class: "file-chip", title: f }, icon("file"), splitPath(f).name, h("span", { class: "muted" }, "copied in"))),
              )
            : null,
        );
      case "progress":
        return h(
          "div",
          { class: "item card progress" },
          h("span", { class: "card-label" }, "What it did"),
          h("ul", { class: "steps" }, ...body.steps.map((s) => h("li", {}, icon("check"), s))),
        );
      case "approval":
        return this.approval(body, agent);
      case "done":
        return h(
          "div",
          { class: "item card done" },
          h("span", { class: "done-text" }, body.text),
          body.changed?.length
            ? h(
                "ul",
                { class: "changed", "aria-label": "What it changed" },
                ...body.changed.map((path) => {
                  const { name, folder } = splitPath(path);
                  return h("li", {}, icon("sheet"), h("span", {}, h("strong", {}, name), folder ? h("span", { class: "muted" }, `in ${folder}`) : null));
                }),
              )
            : null,
          undoSince ? undoControl(agent.id, undoSince, "Undo") : null,
        );
      case "note":
        return h("div", { class: "item note" }, icon(/sleep|asleep|wok|wak|storage/i.test(body.text) ? "moon" : "info"), h("span", {}, body.text));
    }
  }

  /** A question. Approve says what it does ("Send 3 emails"); a
   * confidential one shows only that it needs the person until they
   * choose to see it, so its words aren't on a screen by accident. */
  private approval(a: ApprovalView, agent: AgentView): HTMLElement {
    const pending = a.state.state === "pending";
    const me = app.session?.person.id ?? "";
    const answered = answerLine(a.state, me, clock);
    const hidden = Boolean(a.confidential) && !app.revealed.has(a.id);
    const toggle = a.confidential
      ? button(
          hidden ? "Show what it asks" : "Hide it",
          () => {
            if (hidden) app.revealed.add(a.id);
            else app.revealed.delete(a.id);
            this.focusAfter = `reveal-${a.id}`;
            this.draw();
          },
          "link",
          { "aria-expanded": String(!hidden), "data-key": `reveal-${a.id}` },
        )
      : null;
    let busy = false;
    const answerWith = (yes: boolean) => async () => {
      if (busy) return;
      busy = true;
      this.focusAfter = `answer-${a.id}`;
      await answer(agent.id, agent.name, a, yes);
      busy = false;
    };
    return h(
      "div",
      { class: `item card approval${pending ? " pending" : ""}${a.confidential ? " confidential" : ""}`, "data-approval": a.id },
      h(
        "span",
        { class: "card-label" },
        pending ? h("span", { class: "waiting-dot", "aria-hidden": "true" }) : null,
        pending ? "Needs you" : "Asked you",
        a.confidential ? h("span", { class: "tag" }, icon("lock", "inline"), "Confidential") : null,
      ),
      hidden
        ? h("span", { class: "ask-text" }, pending ? `${agent.name} needs your answer on something confidential.` : "A confidential question.")
        : h("span", { class: "ask-text" }, a.summary),
      !hidden && a.detail ? h("span", { class: "detail" }, a.detail) : null,
      pending && !hidden
        ? h(
            "span",
            { class: "row" },
            button(a.yes_label, answerWith(true), "primary", { "data-key": `yes-${a.id}` }),
            button("Don’t", answerWith(false), "secondary", { "data-key": `no-${a.id}`, "aria-label": `Don’t: ${a.yes_label}` }),
            h("a", { class: "btn link", href: `/agents/${agent.id}/computer` }, "See it on its screen"),
            toggle,
          )
        : toggle
          ? h("span", { class: "row" }, toggle)
          : null,
      answered ? h("span", { class: `answer ${a.state.state}`, tabindex: "-1", "data-key": `answer-${a.id}` }, answered) : null,
    );
  }

  private sidePanel(detail: AgentDetail): HTMLElement[] {
    const agent = detail.agent;
    const c = agent.computer;
    const words = computerWords(c);
    const asleep = c.state === "asleep";
    const sleeper = sleepAction(c);
    const computer = h(
      "section",
      { class: "panel computer-card", "aria-labelledby": "computer-title" },
      thumbnail(words.short, c.state === "running" && agent.status === "working", asleep, true),
      h(
        "div",
        { class: "panel-body" },
        h("h2", { id: "computer-title" }, "Its computer"),
        h("p", { class: "muted" }, words.line),
        h(
          "div",
          { class: "row" },
          asleep
            ? button([icon("expand"), "Wake and open"], async () => {
                if (await control(agent.id, "wake", true)) go(`/agents/${agent.id}/computer`);
              }, "primary", { "data-key": "open-computer" })
            : h("a", { class: "btn primary", href: `/agents/${agent.id}/computer`, "data-key": "open-computer" }, icon("expand"), "Open full screen"),
          agent.status === "taken_over"
            ? button("Hand back", () => control(agent.id, "hand-back"), "secondary", { "data-key": "control" })
            : !asleep && c.state !== "new"
              ? button("Take over", async () => {
                  if (await control(agent.id, "take-over", true)) go(`/agents/${agent.id}/computer`);
                }, "secondary", { "data-key": "control" })
              : null,
          sleeper === "sleep" && agent.status !== "taken_over"
            ? button("Put it to sleep", () => control(agent.id, "sleep"), "link", { "data-key": "sleep" })
            : sleeper === "wake"
              ? button("Wake it", () => control(agent.id, "wake"), "link", { "data-key": "sleep" })
              : null,
        ),
        h(
          "details",
          { class: "about" },
          h("summary", {}, "About your computer"),
          h(
            "ul",
            {},
            h("li", {}, `Runs in: ${c.kind}`),
            h("li", {}, c.contained ? "A security boundary: what happens inside stays inside." : "Not a security boundary: for trying things out only."),
            c.disk_bytes ? h("li", {}, `${size(c.disk_bytes)} on its disk`) : null,
            c.stored ? h("li", {}, `Saved ${ago(c.stored.at, new Date())}: ${size(c.stored.bytes)} in ${c.stored.place}`) : null,
          ),
        ),
      ),
    );
    const reach = h("section", { class: "panel", "aria-labelledby": "reach-title" }, h("h2", { id: "reach-title" }, "What it can reach"), reachList(detail.reach));
    const record = h(
      "section",
      { class: "panel", "aria-labelledby": "record-title" },
      h("h2", { id: "record-title" }, "On the record"),
      detail.record.length
        ? h("ol", { class: "record" }, ...detail.record.map((line) => h("li", {}, h("span", { class: "at" }, clock(line.at)), h("span", {}, line.text))))
        : h("p", { class: "muted" }, "Nothing yet."),
    );
    return [computer, reach, record];
  }

  private async ask(event: Event): Promise<void> {
    event.preventDefault();
    const text = this.input.value.trim();
    show(this.formError);
    if (!text) {
      show(this.formError, notice("Say what you’d like it to do.", "bad"));
      this.input.focus();
      return;
    }
    this.send.disabled = true;
    this.send.textContent = "Sending…";
    try {
      await api<Asked>("POST", agentPath(this.id, "/ask"), { text, via: "web" });
      this.input.value = "";
      await loadDetail(this.id);
      this.list.scrollTop = this.list.scrollHeight;
      app.redraw();
    } catch (error) {
      show(this.formError, notice((error as Error).message, "bad"));
    } finally {
      this.send.disabled = false;
      this.send.textContent = "Send";
      this.input.focus();
    }
  }
}
