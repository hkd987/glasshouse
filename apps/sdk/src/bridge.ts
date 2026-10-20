// The typed bridge client (ADR 0024). An app reaches the system only
// through it: every call goes to the app's own bridge, which relays it to
// glassd, where the manifest, policy, approvals and the audit log decide.
// Refusals come back as results (`ok: false`) the app shows; only a bridge
// that can't be reached is an error, and then reads fall back to what was
// loaded before (the `offline` permission).

import { Cache, type Store, memoryStore } from "./cache.ts";
import type { AppCallResult, AppSession, AppStatus, Tier } from "./manifest.ts";

/** How the client reaches its bridge: same-origin fetch in a page. */
export interface Transport {
  post(endpoint: string, body: unknown): Promise<{ status: number; body: unknown }>;
}

/** The bridge, or the system behind it, can't be reached right now. */
export class Offline extends Error {
  constructor(message = "The system can't be reached right now.") {
    super(message);
    this.name = "Offline";
  }
}

/** The bridge refused the request itself (not an action's refusal). */
export class BridgeError extends Error {
  readonly status: number;
  constructor(status: number, message: string) {
    super(message);
    this.name = "BridgeError";
    this.status = status;
  }
}

export const fetchTransport: Transport = {
  async post(endpoint, body) {
    const response = await fetch(`/_bridge/${endpoint}`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body ?? {}),
      cache: "no-store",
    });
    let parsed: unknown = null;
    try {
      parsed = await response.json();
    } catch {
      parsed = null;
    }
    return { status: response.status, body: parsed };
  },
};

/** A read, and whether it came from what was loaded before. */
export interface Read<T> {
  result: AppCallResult<T>;
  offline: boolean;
  /** When a cached result was loaded (ms since the epoch). */
  loadedAt?: number;
}

export interface BridgeOptions {
  /** The app's id, which keeps its cache apart from other apps'. */
  app: string;
  transport?: Transport;
  store?: Store;
}

function pageStore(): Store {
  try {
    if (typeof localStorage !== "undefined") return localStorage;
  } catch {
    // Storage can be refused (private windows); keep going without it.
  }
  return memoryStore();
}

export class Bridge {
  readonly app: string;
  readonly #transport: Transport;
  readonly #cache: Cache;
  #session: AppSession | null = null;
  /** True while the last request couldn't reach the system. */
  offline = false;

  constructor(options: BridgeOptions) {
    this.app = options.app;
    this.#transport = options.transport ?? fetchTransport;
    this.#cache = new Cache(options.store ?? pageStore(), options.app);
  }

  get session(): AppSession | null {
    return this.#session;
  }

  async #post<T>(endpoint: string, body: unknown): Promise<T> {
    let reply: { status: number; body: unknown };
    try {
      reply = await this.#transport.post(endpoint, body);
    } catch {
      this.offline = true;
      throw new Offline();
    }
    if (reply.status === 503) {
      this.offline = true;
      throw new Offline();
    }
    this.offline = false;
    if (reply.status !== 200) {
      const error = (reply.body as { error?: string } | null)?.error;
      throw new BridgeError(reply.status, error ?? `the bridge answered ${reply.status}`);
    }
    return reply.body as T;
  }

  /**
   * Opens the app for the signed-in user: who it acts for, and each
   * declared action's tier today. Offline, the last session seen.
   */
  async open(): Promise<AppSession> {
    try {
      this.#session = await this.#post<AppSession>("session", {});
      this.#cache.put("session", this.#session);
    } catch (error) {
      const cached = error instanceof Offline ? this.#cache.get<AppSession>("session") : null;
      if (!cached) throw error;
      this.#session = cached.value;
    }
    return this.#session;
  }

  /** How `action` runs for this user today; undefined if not declared. */
  tier(action: string): Tier | undefined {
    return this.#session?.actions.find((a) => a.name === action)?.tier;
  }

  /** Whether each call of `action` waits for the user on the desktop. */
  asks(action: string): boolean {
    return this.tier(action) === "ask";
  }

  /** Runs one action. Throws `Offline` if the system can't be reached. */
  async call<T = unknown>(action: string, input: unknown = {}): Promise<AppCallResult<T>> {
    return this.#post<AppCallResult<T>>("call", { action, input });
  }

  /**
   * A read (a side-effect-free action): kept on the device when it works,
   * and shown from there when the system can't be reached.
   */
  async read<T = unknown>(action: string, input: unknown = {}): Promise<Read<T>> {
    const key = `read:${action}:${JSON.stringify(input)}`;
    try {
      const result = await this.call<T>(action, input);
      if (result.ok) this.#cache.put(key, result);
      return { result, offline: false };
    } catch (error) {
      const cached = error instanceof Offline ? this.#cache.get<AppCallResult<T>>(key) : null;
      if (!cached) throw error;
      return { result: cached.value, offline: true, loadedAt: cached.at };
    }
  }

  /**
   * Whether the session is open and what it waits on the user for. With
   * `waitSecs`, waits (25 s at most) for the person's data in this app to
   * change after `since`, a cursor from an earlier status: `changed` then
   * names this app. Ids only; read what changed with one call (ADR 0030).
   */
  async status(wait?: { waitSecs: number; since: number }): Promise<AppStatus> {
    const status = await this.#post<AppStatus>("status", wait ? { wait_secs: wait.waitSecs, since: wait.since } : {});
    // A new policy reaches open apps: `tier` and `asks` answer by it.
    if (this.#session && status.actions) {
      this.#session = { ...this.#session, actions: status.actions };
      this.#cache.put("session", this.#session);
    }
    return status;
  }

  /**
   * Live editing of an item this session opened (ADR 0035 §3): `push`
   * sends a Yjs update; `pull` returns the updates after a cursor, waiting
   * (25 s at most) for new ones. Not an action: glassd allows it only for
   * an item the session opened, in the role it opened with, and refuses
   * it otherwise (a `BridgeError`). Throws `Offline` like any request.
   */
  async live<T = LivePulled>(body: LiveRequest): Promise<T> {
    return this.#post<T>("live", body);
  }
}

/**
 * What a page sends `/_bridge/live`: one update, or a wait for others'.
 * `client` names the page (a random id made once per page load, up to 64
 * letters, digits and `-`): a pull leaves out only that page's own pushes,
 * so two windows of one app on one bridge see each other's edits.
 */
export type LiveRequest =
  | { op: "push"; doc: string; update: string; client?: string }
  | { op: "pull"; doc: string; since: number; wait_secs: number; client?: string };

/** An update someone else's computer (or an agent) made, base64. */
export interface LiveUpdate {
  update: string;
  by: string;
  by_name: string;
}

/** A push's answer: where the item's log is now. */
export interface LivePushed {
  cursor: number;
}

/** A pull's answer: the updates after `since`, and the cursor to pass next. */
export interface LivePulled {
  updates: LiveUpdate[];
  cursor: number;
}

/**
 * Lets the app's own files load without a connection: a service worker
 * keeps the last copy of each (UI-5). Best effort.
 */
export function keepFilesOffline(script = "./sw.js"): void {
  if (typeof navigator === "undefined" || !("serviceWorker" in navigator)) return;
  navigator.serviceWorker.register(script).catch(() => undefined);
}
