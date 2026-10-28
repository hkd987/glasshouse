// Its computer, full screen (ADR 0065 §4, §12): the screen as a terminal,
// read from where the last read ended and appended; Take over pauses the
// agent and gives the person a command line, on the record; Hand back
// returns it. A computer with a display of its own, a Glasshouse OS, shows
// its desktop first, and Take over gives the person its pointer and
// keyboard (ADR 0072). This view may be technical; it says plainly that the
// agent waits while the person has it.

import { h, show } from "@glasshouse/sdk";

import { type AgentView, type Screen, SignedOut, agentPath, api } from "./api.ts";
import { agent, app, control, loadDetail, undoControl } from "./app.ts";
import { sleep } from "./events.ts";
import { DesktopView } from "./desktop.ts";
import { computerWords, undoable } from "./model.ts";
import { ScreenBuffer } from "./screen.ts";
import { button, focusHeading, icon, keepFocus, notice } from "./ui.ts";

/** How often it reads the screen: while something can change, and while
 * it sleeps. The events stream also asks for a read when it changes. */
const READ_MS = 1000;
const QUIET_MS = 5000;

export class ComputerScreen {
  readonly id: string;
  private readonly root: HTMLElement;
  private readonly bar = h("div", { class: "cbar" });
  private readonly note = h("div", { class: "control-note-box" });
  private readonly term = h("pre", { class: "term", tabindex: "0", "aria-label": "Its screen", "aria-describedby": "screen-state" });
  private readonly frameHead = h("div", { class: "screen-head" });
  private readonly cover = h("div", { class: "screen-cover-box" });
  private readonly line = h("input", { id: "line", type: "text", autocomplete: "off", autocapitalize: "off", spellcheck: "false", enterkeyhint: "send" });
  private readonly run = h("button", { type: "submit", class: "btn primary" }, "Run");
  private readonly lineError = h("div", { class: "form-error" });
  private readonly inputBox: HTMLElement;
  private readonly buffer = new ScreenBuffer();
  private screen: Screen | null = null;
  private readonly abort = new AbortController();
  private wake: AbortController | null = null;
  /** One screen request at a time, so an answer can't arrive out of order. */
  private chain: Promise<unknown> = Promise.resolve();
  /** Its desktop, for a computer with a display; and which view shows. */
  private desktop: DesktopView | null = null;
  private mode: "desktop" | "terminal" = "desktop";
  private readonly body = h("div", { class: "screen-body" });

  constructor(main: HTMLElement, id: string) {
    this.id = id;
    this.inputBox = h(
      "form",
      { class: "term-input", onsubmit: (e: Event) => void this.type(e) },
      h("label", { for: "line" }, "Type a command"),
      h("span", { class: "prompt", "aria-hidden": "true" }, "$"),
      this.line,
      this.run,
    );
    this.root = h(
      "div",
      { class: "computer" },
      this.bar,
      this.note,
      h(
        "div",
        { class: "screen-wrap" },
        h("div", { class: "screen-frame" }, this.frameHead, this.body, this.inputBox, this.lineError),
      ),
      h("p", { class: "sleep-line" }, "Your computer sleeps after a while with nothing to do: saved to your company’s storage, nothing left running."),
    );
    show(this.body, this.term, this.cover);
    show(main, this.root);
    this.term.textContent = "Connecting to its screen…";
  }

  /** The desktop, made the first time the computer says it has one. */
  private desktopOf(): DesktopView {
    if (!this.desktop) {
      const desktop = new DesktopView(this.id);
      desktop.onError = (message) => show(this.lineError, notice(message, "bad"));
      this.body.prepend(desktop.element);
      desktop.start();
      this.desktop = desktop;
    }
    return this.desktop;
  }

  async open(): Promise<void> {
    try {
      if (!app.details.has(this.id)) await loadDetail(this.id);
      this.draw();
      const view = agent(this.id);
      if (view && this.yours(view)) this.line.focus();
      else focusHeading(this.root);
      void this.loop();
    } catch (error) {
      show(this.bar, h("a", { class: "back", href: "/" }, icon("back"), "All agents"));
      show(this.note, notice((error as Error).message, "bad"));
    }
  }

  /** Draws the bar and what depends on who has the computer. */
  draw(): void {
    const view = agent(this.id);
    if (!view) return;
    const yours = this.yours(view);
    const state = this.screen?.state ?? view.computer.state;
    const desktop = view.computer.display && state !== "asleep" ? this.desktopOf() : null;
    const showDesktop = desktop !== null && this.mode === "desktop";
    const words = computerWords({ ...view.computer, state });
    document.title = "Your computer · Glasshouse Agents";
    const detail = app.details.get(this.id);
    const target = detail ? undoable(detail.messages) : null;
    keepFocus(this.bar, () =>
      show(
        this.bar,
        h("a", { class: "back", href: `/agents/${view.id}` }, icon("back"), view.name),
        h(
          "div",
          { class: "ctitle" },
          h("h1", {}, "Your computer"),
          h("span", { class: `chip ${yours ? "yours" : "watching"}` }, yours ? "You’re in control" : "Watching"),
        ),
        h(
          "div",
          { class: "row" },
          state === "asleep"
            ? null
            : yours
              ? button("Hand back", () => this.control("hand-back"), "go", { "data-key": "control" })
              : button("Take over", () => this.control("take-over"), "take", { "data-key": "control" }),
          target && !yours ? undoControl(view.id, target.since, "Undo this task", "dark") : null,
        ),
      ),
    );
    show(
      this.note,
      yours
        ? h(
            "p",
            { class: "control-note", role: "status" },
            showDesktop
              ? "Your agents wait for you: asking them is refused until you hand your computer back. It's yours: click and type on it. Taking it over and handing it back are on the record."
              : "Your agents wait for you: asking them is refused until you hand your computer back. It's yours: type below. What you do here is on the record.",
          )
        : null,
    );
    show(
      this.frameHead,
      desktop
        ? h(
            "span",
            { class: "screen-tabs", role: "group", "aria-label": "What to show" },
            button("Desktop", () => this.choose("desktop"), showDesktop ? "tab on" : "tab", { "aria-pressed": String(showDesktop) }),
            button("Terminal", () => this.choose("terminal"), showDesktop ? "tab" : "tab on", { "aria-pressed": String(!showDesktop) }),
          )
        : h("span", {}, "Terminal"),
      h("span", { id: "screen-state" }, `${words.short} · ${view.computer.kind}${view.computer.contained ? "" : " (not a security boundary)"}`),
    );
    show(
      this.cover,
      state === "asleep"
        ? h(
            "div",
            { class: "screen-cover" },
            icon("moon", "moon"),
            h("span", {}, "Asleep, saved to storage. Waking it picks up where it left off."),
            button("Wake it", () => this.control("wake"), "take", { "data-key": "wake" }),
          )
        : state === "waking" || state === "starting"
          ? h("div", { class: "screen-cover light" }, h("span", {}, words.line))
          : null,
    );
    this.inputBox.hidden = !yours || showDesktop;
    this.term.hidden = showDesktop;
    if (desktop) {
      desktop.element.hidden = !showDesktop;
      desktop.setYours(yours && showDesktop);
    }
    this.root.classList.toggle("is-yours", yours);
    this.root.classList.toggle("has-desktop", showDesktop);
  }

  private choose(mode: "desktop" | "terminal"): void {
    this.mode = mode;
    this.draw();
  }

  destroy(): void {
    this.desktop?.stop();
    this.abort.abort();
    this.wake?.abort();
    this.root.remove();
  }

  /** Reads now, as when the events stream says it changed. */
  poke(): void {
    this.wake?.abort();
    this.desktop?.poke();
  }

  private yours(view: AgentView): boolean {
    return view.status === "taken_over" || Boolean(this.screen?.taken_over);
  }

  private async loop(): Promise<void> {
    const { signal } = this.abort;
    while (!signal.aborted) {
      try {
        await this.read();
      } catch (error) {
        if (signal.aborted || error instanceof SignedOut) return;
      }
      const busy = this.screen && this.screen.state !== "asleep";
      this.wake = new AbortController();
      const both = AbortSignal.any([signal, this.wake.signal]);
      await sleep(document.hidden ? QUIET_MS : busy ? READ_MS : QUIET_MS, both);
    }
  }

  /** One read, in turn with the others; a screen that started over is
   * read again from its start. */
  private read(): Promise<void> {
    const next = this.chain.then(async () => {
      const first = this.screen === null;
      const screen = await api<Screen>("GET", agentPath(this.id, `/screen?since=${this.buffer.offset}`), undefined, this.abort.signal);
      this.show(screen, first);
      if (this.buffer.offset > screen.offset) {
        this.buffer.clear();
        this.show(await api<Screen>("GET", agentPath(this.id, "/screen?since=0"), undefined, this.abort.signal), true);
      }
    });
    this.chain = next.catch(() => undefined);
    return next;
  }

  private show(screen: Screen, first: boolean): void {
    const was = this.screen;
    this.screen = screen;
    const term = this.term;
    const atBottom = term.scrollHeight - term.scrollTop - term.clientHeight < 40;
    const result = screen.offset < this.buffer.offset ? "reset" : this.buffer.apply(screen);
    if (result !== "nothing" && result !== "reset") {
      term.textContent = this.buffer.text || " ";
      if (atBottom || first) term.scrollTop = term.scrollHeight;
    } else if (first && !this.buffer.text) {
      term.textContent = "Nothing on its screen yet.";
    }
    if (!was || was.taken_over !== screen.taken_over || was.state !== screen.state) this.draw();
  }

  private async control(what: "take-over" | "hand-back" | "wake"): Promise<void> {
    const view = await control(this.id, what);
    if (!view) return;
    if (this.screen) this.screen = { ...this.screen, taken_over: view.status === "taken_over", state: view.computer.state };
    this.draw();
    this.poke();
    if (what === "take-over" && this.desktop && this.mode === "desktop") this.desktop.element.focus({ preventScroll: true });
    else if (what === "take-over") this.line.focus();
    else this.bar.querySelector<HTMLElement>('[data-key="control"]')?.focus();
  }

  /** A command line, run in its computer as the person (taken over only). */
  private async type(event: Event): Promise<void> {
    event.preventDefault();
    const line = this.line.value;
    show(this.lineError);
    if (!line.trim()) return;
    this.run.disabled = true;
    try {
      const next = this.chain.then(async () => {
        const screen = await api<Screen>("POST", agentPath(this.id, "/input"), { line });
        this.show(screen, false);
      });
      this.chain = next.catch(() => undefined);
      await next;
      this.line.value = "";
      this.term.scrollTop = this.term.scrollHeight;
    } catch (error) {
      show(this.lineError, notice((error as Error).message, "bad"));
    } finally {
      this.run.disabled = false;
      this.line.focus();
    }
  }
}
