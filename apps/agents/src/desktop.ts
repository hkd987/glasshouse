// A computer's own desktop (ADR 0072): the Glasshouse OS it runs, shown
// as its display draws it, and, while the person has it, driven with their
// pointer and keys. Watching, the picture is all there is; the computer
// page's Take over gives the person the pointer and keyboard.

import { h } from "@glasshouse/sdk";

import { type Display, type DisplayInput, agentPath, api } from "./api.ts";
import { sleep } from "./events.ts";

/** How often it looks for a new picture: watched, and in a hidden tab. */
const LOOK_MS = 350;
const HIDDEN_MS = 3000;
/** Pointer moves are sent at most this often; buttons and keys at once. */
const MOVE_MS = 40;

/** Where a point on the picture as shown lands on the display. */
export function toDisplay(
  clientX: number,
  clientY: number,
  rect: { left: number; top: number; width: number; height: number },
  width: number,
  height: number,
): { x: number; y: number } | null {
  if (rect.width <= 0 || rect.height <= 0 || width <= 0 || height <= 0) return null;
  const x = Math.floor(((clientX - rect.left) * width) / rect.width);
  const y = Math.floor(((clientY - rect.top) * height) / rect.height);
  if (x < 0 || y < 0 || x >= width || y >= height) return null;
  return { x, y };
}

/** A browser's mouse button to the display's. */
export function buttonOf(button: number): "left" | "middle" | "right" | null {
  return button === 0 ? "left" : button === 1 ? "middle" : button === 2 ? "right" : null;
}

export class DesktopView {
  readonly element: HTMLElement;
  private readonly picture = h("img", { class: "desktop-picture", alt: "", draggable: "false" }) as HTMLImageElement;
  private readonly status = h("p", { class: "desktop-status", role: "status" }, "Connecting to its display…");
  private version: string | null = null;
  private width = 0;
  private height = 0;
  private yours = false;
  private queue: DisplayInput[] = [];
  private flushing: Promise<void> = Promise.resolve();
  private moveTimer: number | null = null;
  private pendingMove: { x: number; y: number } | null = null;
  private wake: AbortController | null = null;
  private readonly abort = new AbortController();
  /** Said when sending fails: the computer page shows it. */
  onError: (message: string) => void = () => undefined;

  private readonly id: string;

  constructor(id: string) {
    this.id = id;
    this.element = h(
      "div",
      {
        class: "desktop",
        tabindex: "0",
        role: "application",
        "aria-label": "Your computer's desktop",
        "aria-roledescription": "remote desktop",
      },
      this.picture,
      this.status,
    );
    const el = this.element;
    el.addEventListener("mousemove", (e) => this.pointer(e));
    el.addEventListener("mousedown", (e) => this.press(e, true));
    el.addEventListener("mouseup", (e) => this.press(e, false));
    el.addEventListener("contextmenu", (e) => this.yours && e.preventDefault());
    el.addEventListener("wheel", (e) => this.wheel(e), { passive: false });
    el.addEventListener("keydown", (e) => this.key(e, true));
    el.addEventListener("keyup", (e) => this.key(e, false));
  }

  /** Whether the person has it: their pointer and keys go to it. */
  setYours(yours: boolean): void {
    this.yours = yours;
    this.element.classList.toggle("is-yours", yours);
    this.element.setAttribute("aria-label", yours ? "Your computer's desktop: you have its pointer and keyboard" : "Your computer's desktop, watching");
    if (yours) this.element.focus({ preventScroll: true });
  }

  start(): void {
    void this.loop();
  }

  stop(): void {
    this.abort.abort();
    this.wake?.abort();
  }

  /** Looks now, as when the person did something on it. */
  poke(): void {
    this.wake?.abort();
  }

  private async loop(): Promise<void> {
    const { signal } = this.abort;
    while (!signal.aborted) {
      try {
        const since = this.version ? `?since=${encodeURIComponent(this.version)}` : "";
        const display = await api<Display>("GET", agentPath(this.id, `/display${since}`), undefined, signal);
        this.draw(display);
      } catch (error) {
        if (signal.aborted) return;
        this.status.hidden = false;
        this.status.textContent = (error as Error).message;
      }
      this.wake = new AbortController();
      await sleep(document.hidden ? HIDDEN_MS : LOOK_MS, AbortSignal.any([signal, this.wake.signal]));
    }
  }

  private draw(display: Display): void {
    this.width = display.width;
    this.height = display.height;
    if (display.png) {
      this.picture.src = `data:image/png;base64,${display.png}`;
      this.picture.alt = "What your computer's display shows";
      this.version = display.version;
      this.status.hidden = true;
    }
  }

  private at(e: MouseEvent): { x: number; y: number } | null {
    return toDisplay(e.clientX, e.clientY, this.picture.getBoundingClientRect(), this.width, this.height);
  }

  private pointer(e: MouseEvent): void {
    if (!this.yours) return;
    const at = this.at(e);
    if (!at) return;
    this.pendingMove = at;
    if (this.moveTimer === null) {
      this.moveTimer = window.setTimeout(() => {
        this.moveTimer = null;
        this.flushMove();
        this.send();
      }, MOVE_MS);
    }
  }

  private flushMove(): void {
    if (this.pendingMove) {
      this.queue.push({ kind: "move", ...this.pendingMove });
      this.pendingMove = null;
    }
  }

  private press(e: MouseEvent, down: boolean): void {
    if (!this.yours) return;
    const button = buttonOf(e.button);
    const at = this.at(e);
    if (!button || !at) return;
    e.preventDefault();
    this.element.focus({ preventScroll: true });
    this.pendingMove = null;
    this.queue.push({ kind: "move", ...at }, { kind: "button", button, down });
    this.send();
  }

  private wheel(e: WheelEvent): void {
    if (!this.yours) return;
    e.preventDefault();
    const dy = Math.sign(e.deltaY);
    if (dy === 0) return;
    this.flushMove();
    this.queue.push({ kind: "wheel", dy });
    this.send();
  }

  private key(e: KeyboardEvent, down: boolean): void {
    if (!this.yours || !e.code) return;
    // Everything typed goes to the computer, browser shortcuts included.
    e.preventDefault();
    this.flushMove();
    this.queue.push({ kind: "key", code: e.code, down });
    this.send();
  }

  /** Sends what's queued, in order, one call at a time. */
  private send(): void {
    this.flushing = this.flushing.then(async () => {
      while (this.queue.length) {
        const events = this.queue.splice(0, 64);
        try {
          await api<unknown>("POST", agentPath(this.id, "/display"), { events });
        } catch (error) {
          this.queue = [];
          this.onError((error as Error).message);
          return;
        }
      }
      this.poke();
    });
  }
}
