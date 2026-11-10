// A doc's live session (ADR 0035 §3), without a DOM: the page's Yjs
// document, kept in step with everyone else's through the bridge's
// `/_bridge/live`, never through actions.
//
// - What this page changes is merged (Y.mergeUpdates) and pushed at most
//   once per interval (400 ms), one push at a time.
// - One pull waits for what others changed (25 s at most) and applies it
//   with the REMOTE origin, so it is never pushed back.
// - A session glassd no longer has open (the bridge refuses the doc) is
//   opened again: its state merged, and whatever the system lacks pushed.
// - Unreachable, it keeps what's unsent in memory, says so, and tries
//   again; the page stops taking edits meanwhile.
// - Viewers and commenters never push: glassd would drop it anyway.

import { fromBase64, toBase64 } from "lib0/buffer";
import * as Y from "yjs";

/** The origin of every update that came from elsewhere: never pushed back. */
export const REMOTE = Symbol("remote");

/** The XmlFragment y-prosemirror binds the editor to (ADR 0035 §1). */
export const FRAGMENT = "prosemirror";

export const PUSH_INTERVAL_MS = 400;
export const PULL_WAIT_SECS = 25;

export interface Pulled {
  updates: { update: string; by: string; by_name: string }[];
  cursor: number;
}

/** How the session reaches the system: the bridge in the page, a fake in tests. */
export interface Channel {
  push(update: string): Promise<{ cursor: number }>;
  pull(since: number, waitSecs: number): Promise<Pulled>;
  /** doc.open again: its state and cursor, or null when it can't be opened. */
  reopen(): Promise<{ state: string; cursor: number } | null>;
}

export type Health = "live" | "sending" | "offline" | "refused";

export interface LiveOptions {
  doc: Y.Doc;
  channel: Channel;
  /** From doc.open. */
  cursor: number;
  /** The state doc.open returned, applied already: what the system has. */
  state?: string;
  readOnly?: boolean;
  intervalMs?: number;
  waitSecs?: number;
  /** How long to wait before trying again when the system can't be reached. */
  retryMs?: number;
  /** Someone else's change arrived. */
  onRemote?: (from: { by: string; by_name: string }[]) => void;
  /** Whether it is caught up, sending, can't reach the system, or was refused (with why). */
  onHealth?: (health: Health, why?: string) => void;
  /** The doc was opened again (its comments and versions may be newer). */
  onReopen?: () => void;
}

/** Why a request failed, as the page tells it apart: the SDK's errors, by name, so this stays DOM-free. */
export function failureKind(error: unknown): "offline" | "not-open" | "refused" {
  const e = error as { name?: unknown; message?: unknown; status?: unknown } | null;
  if (e?.name === "Offline") return "offline";
  const message = typeof e?.message === "string" ? e.message : "";
  if (/\bnot open|isn't open|is not open|wasn't opened|not opened|open it first|no live session/i.test(message) || e?.status === 409) return "not-open";
  if (e?.name === "BridgeError") return "refused";
  // Anything else (a broken connection mid-answer) is treated as unreachable.
  return "offline";
}

const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

export class LiveSync {
  readonly doc: Y.Doc;
  readonly #channel: Channel;
  readonly #readOnly: boolean;
  readonly #interval: number;
  readonly #wait: number;
  readonly #retry: number;
  readonly #o: LiveOptions;
  /** Where the pull reads from next. */
  cursor: number;
  health: Health = "live";
  /** Pushes sent (tests count them). */
  pushes = 0;
  /** When an edit last went out or came in (ms since the epoch): the doc is being typed in. */
  activeAt = 0;
  #pending: Uint8Array[] = [];
  #sending = false;
  #timer: ReturnType<typeof setTimeout> | null = null;
  #last = Number.NEGATIVE_INFINITY;
  #stopped = false;
  #pulling = false;
  #reopening: Promise<boolean> | null = null;
  readonly #onUpdate = (update: Uint8Array, origin: unknown) => {
    if (origin === REMOTE || this.#stopped || this.#readOnly) return;
    this.#pending.push(update);
    this.#schedule();
  };

  constructor(o: LiveOptions) {
    this.doc = o.doc;
    this.#channel = o.channel;
    this.cursor = o.cursor;
    this.#readOnly = Boolean(o.readOnly);
    this.#interval = o.intervalMs ?? PUSH_INTERVAL_MS;
    this.#wait = o.waitSecs ?? PULL_WAIT_SECS;
    this.#retry = o.retryMs ?? 3000;
    this.#o = o;
    this.doc.on("update", this.#onUpdate);
  }

  /** Starts the one pull that waits for others' changes. */
  start(): this {
    if (!this.#pulling) void this.#pullLoop();
    return this;
  }

  get stopped(): boolean {
    return this.#stopped;
  }

  /** Whether something this page changed hasn't reached the system yet. */
  get unsent(): boolean {
    return this.#pending.length > 0 || this.#sending;
  }

  /**
   * For a doc being closed: sends what's unsent, then stops. With the
   * system away that isn't now: it keeps trying, as it does while the doc
   * is open, for as long as the page is. Closing used to send once and
   * stop, so what was typed just before the system went away was thrown
   * out with the doc. Whether it all went (not, if the system refused it).
   */
  async finish(): Promise<boolean> {
    for (;;) {
      if (this.#stopped) return this.health !== "refused" && !this.unsent;
      if (!this.unsent) {
        this.stop();
        return true;
      }
      await this.flush();
      if (this.unsent && !this.#stopped) await new Promise((resolve) => setTimeout(resolve, this.#retry));
    }
  }

  /** Stops pushing and pulling; a pull still waiting is ignored when it answers. */
  stop(): void {
    this.#stopped = true;
    this.doc.off("update", this.#onUpdate);
    if (this.#timer) clearTimeout(this.#timer);
    this.#timer = null;
  }

  #setHealth(health: Health, why?: string): void {
    if (this.health === health && health !== "refused") return;
    this.health = health;
    this.#o.onHealth?.(health, why);
  }

  #schedule(delay?: number): void {
    if (this.#timer || this.#sending || this.#stopped || !this.#pending.length) return;
    const wait = delay ?? Math.max(0, this.#last + this.#interval - Date.now());
    this.#timer = setTimeout(() => {
      this.#timer = null;
      void this.flush();
    }, wait);
  }

  /** Pushes what's waiting now, merged into one update. */
  async flush(): Promise<void> {
    if (this.#sending || this.#stopped || !this.#pending.length) return;
    if (this.#timer) {
      clearTimeout(this.#timer);
      this.#timer = null;
    }
    const update = this.#pending.length === 1 ? (this.#pending[0] as Uint8Array) : Y.mergeUpdates(this.#pending);
    this.#pending = [];
    this.#sending = true;
    this.#last = Date.now();
    this.#setHealth("sending");
    let retry: number | undefined;
    try {
      this.pushes += 1;
      await this.#channel.push(toBase64(update));
      this.activeAt = Date.now();
      this.#sending = false;
      if (!this.#pending.length) this.#setHealth("live");
    } catch (error) {
      // Kept, ahead of anything newer, and sent again.
      this.#pending.unshift(update);
      const kind = failureKind(error);
      if (kind === "not-open") {
        this.#sending = false;
        if (await this.#reopen()) retry = 0;
        else retry = this.#retry;
      } else if (kind === "offline") {
        this.#setHealth("offline");
        retry = this.#retry;
      } else {
        // Refused: glassd won't take edits from this session (a role
        // changed, say). Nothing more is sent; the page says why.
        this.#pending = [];
        this.#setHealth("refused", (error as Error).message);
        this.stop();
      }
    } finally {
      this.#sending = false;
    }
    if (this.#pending.length) this.#schedule(retry);
  }

  /** Opens the doc again, takes in its state, and queues what the system lacks. Once at a time. */
  #reopen(): Promise<boolean> {
    this.#reopening ??= (async () => {
      try {
        const opened = await this.#channel.reopen();
        if (!opened || this.#stopped) return false;
        const state = fromBase64(opened.state);
        Y.applyUpdate(this.doc, state, REMOTE);
        this.cursor = opened.cursor;
        if (!this.#readOnly) {
          const missing = Y.encodeStateAsUpdate(this.doc, Y.encodeStateVectorFromUpdate(state));
          if (this.#pending.length || hasStructs(missing)) this.#pending = [missing];
        }
        this.#o.onReopen?.();
        this.#setHealth(this.#pending.length ? "sending" : "live");
        return true;
      } catch (error) {
        this.#setHealth(failureKind(error) === "offline" ? "offline" : "refused", (error as Error).message);
        return false;
      } finally {
        this.#reopening = null;
      }
    })();
    return this.#reopening;
  }

  async #pullLoop(): Promise<void> {
    this.#pulling = true;
    try {
      while (!this.#stopped) {
        try {
          const since = this.cursor;
          const pulled = await this.#channel.pull(since, this.#wait);
          if (this.#stopped) return;
          // A reopen meanwhile moved the cursor; this answer is older.
          if (this.cursor !== since) continue;
          this.applyRemote(pulled);
          if (this.health === "offline") this.#setHealth(this.unsent ? "sending" : "live");
          if (this.#pending.length) this.#schedule();
        } catch (error) {
          if (this.#stopped) return;
          const kind = failureKind(error);
          if (kind === "not-open") {
            if (!(await this.#reopen())) await sleep(this.#retry);
            else if (this.#pending.length) this.#schedule(0);
          } else if (kind === "offline") {
            this.#setHealth("offline");
            await sleep(this.#retry);
          } else {
            this.#setHealth("refused", (error as Error).message);
            await sleep(this.#retry * 4);
          }
        }
      }
    } finally {
      this.#pulling = false;
    }
  }

  /** Applies a pull's updates as from elsewhere, and moves the cursor on. */
  applyRemote(pulled: Pulled): void {
    const from: { by: string; by_name: string }[] = [];
    if (pulled.updates.length) {
      Y.transact(
        this.doc,
        () => {
          for (const u of pulled.updates) {
            try {
              Y.applyUpdate(this.doc, fromBase64(u.update), REMOTE);
              from.push({ by: u.by, by_name: u.by_name });
            } catch {
              // An update that doesn't decode is skipped; the next open has the whole state.
            }
          }
        },
        REMOTE,
      );
    }
    if (Number.isFinite(pulled.cursor)) this.cursor = pulled.cursor;
    if (from.length) {
      this.activeAt = Date.now();
      this.#o.onRemote?.(from);
    }
  }
}

/** Whether an update adds anything (not just the delete set every diff carries). */
function hasStructs(update: Uint8Array): boolean {
  try {
    const decoded = Y.decodeUpdate(update);
    return decoded.structs.length > 0 || decoded.ds.clients.size > 0;
  } catch {
    return true;
  }
}

// --- Comment anchors ---------------------------------------------------

/** A relative position as `doc.comment` takes it: base64 of Y.encodeRelativePosition. */
export function encodeAnchor(position: Y.RelativePosition): string {
  return toBase64(Y.encodeRelativePosition(position));
}

/** A comment's anchor back as a relative position; null when it doesn't decode. */
export function decodeAnchor(path: unknown): Y.RelativePosition | null {
  if (typeof path !== "string" || !path || path.length > 4096 || !/^[A-Za-z0-9+/]+={0,2}$/.test(path)) return null;
  try {
    return Y.decodeRelativePosition(fromBase64(path));
  } catch {
    return null;
  }
}

/** The state doc.open returned, applied as from elsewhere. */
export function applyState(doc: Y.Doc, state: string): void {
  if (state) Y.applyUpdate(doc, fromBase64(state), REMOTE);
}
