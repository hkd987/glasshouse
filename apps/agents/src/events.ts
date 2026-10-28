// Live updates: a long poll of `GET /api/v1/events?since=<cursor>`, which
// answers when something changed (or after 25 s with nothing). What
// changed is refetched by the page; nothing here touches the DOM, so the
// loop and its backoff are unit tested with a fake front.

import type { Events } from "./api.ts";

/** What to refetch. */
export type Change = { kind: "nothing" } | { kind: "all" } | { kind: "agents"; ids: string[] };

/** What an answer means, given the cursor it was asked from. The same
 * cursor back is a quiet 25 s; a cursor that went back means the front
 * started over, and an empty list with a new cursor means everything. */
export function whatChanged(since: number, events: Events): Change {
  if (events.cursor === since) return { kind: "nothing" };
  if (events.cursor < since || events.agents.length === 0) return { kind: "all" };
  return { kind: "agents", ids: [...new Set(events.agents)] };
}

/** How long to wait after `failures` failed polls in a row: nothing
 * after the first answer, then 1 s, 2 s, 4 s … up to 30 s, each a little
 * shorter at random so many pages don't come back at once. */
export function backoff(failures: number, random: () => number = Math.random): number {
  if (failures <= 0) return 0;
  const base = Math.min(MAX_WAIT_MS, 1000 * 2 ** (failures - 1));
  return Math.round(base * (0.75 + 0.25 * random()));
}

export const MAX_WAIT_MS = 30_000;
/** A front that answers at once with nothing new isn't asked again sooner
 * than this. */
export const MIN_GAP_MS = 1000;

export interface Front {
  /** One long poll. Throws when the front can't be reached or refuses. */
  poll(since: number, signal: AbortSignal): Promise<Events>;
  /** Refetch what changed. */
  changed(change: Change): Promise<void> | void;
  /** Whether the front is answering: the page says so after two misses. */
  online(yes: boolean): void;
  /** Whether an error ends the loop (signed out). */
  fatal?(error: unknown): boolean;
  sleep(ms: number, signal: AbortSignal): Promise<void>;
  now(): number;
  random?(): number;
}

export class EventLoop {
  /** The last cursor the front gave. */
  cursor: number;
  failures = 0;
  private readonly front: Front;
  private abort: AbortController | null = null;

  constructor(front: Front, cursor = 0) {
    this.front = front;
    this.cursor = cursor;
  }

  get running(): boolean {
    return this.abort !== null;
  }

  /** Polls until stopped. Resolves when it stops. */
  async run(): Promise<void> {
    if (this.abort) return;
    const abort = new AbortController();
    this.abort = abort;
    const { signal } = abort;
    while (!signal.aborted) {
      const asked = this.front.now();
      try {
        const events = await this.front.poll(this.cursor, signal);
        if (signal.aborted) break;
        if (this.failures >= 2) this.front.online(true);
        // After a miss, whatever moved is refetched whole.
        const change = this.failures > 0 && events.cursor !== this.cursor ? { kind: "all" as const } : whatChanged(this.cursor, events);
        if (change.kind !== "nothing") await this.front.changed(change);
        // Only once it's refetched: a refetch that fails is asked again.
        this.failures = 0;
        this.cursor = events.cursor;
        if (change.kind === "nothing" && this.front.now() - asked < MIN_GAP_MS) await this.front.sleep(MIN_GAP_MS, signal);
      } catch (error) {
        if (signal.aborted) break;
        if (this.front.fatal?.(error)) break;
        this.failures += 1;
        if (this.failures === 2) this.front.online(false);
        await this.front.sleep(backoff(this.failures, this.front.random), signal);
      }
    }
    if (this.abort === abort) this.abort = null;
  }

  stop(): void {
    this.abort?.abort();
    this.abort = null;
  }
}

/** A sleep that ends early when the signal aborts. */
export function sleep(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    if (signal.aborted || ms <= 0) return resolve();
    const timer = setTimeout(done, ms);
    signal.addEventListener("abort", done, { once: true });
    function done(): void {
      clearTimeout(timer);
      signal.removeEventListener("abort", done);
      resolve();
    }
  });
}
