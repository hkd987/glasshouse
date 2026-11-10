// What an app has loaded, kept on the device so it can show it without a
// connection (UI-5, the `offline` permission). Reads only: nothing here is
// ever sent anywhere, and a call that changes something is never queued.

/** The storage a cache sits on: `localStorage` in a page, a Map in tests. */
export interface Store {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
  readonly length: number;
  key(index: number): string | null;
}

/** An in-memory store, for tests and for when the page has no storage. */
export function memoryStore(): Store {
  const map = new Map<string, string>();
  return {
    getItem: (key) => map.get(key) ?? null,
    setItem: (key, value) => void map.set(key, value),
    removeItem: (key) => void map.delete(key),
    get length() {
      return map.size;
    },
    key: (index) => [...map.keys()][index] ?? null,
  };
}

interface Entry<T> {
  at: number;
  value: T;
}

/** Most entries one app keeps; the oldest go first. */
export const MAX_ENTRIES = 200;

/**
 * When an entry was kept, from its first bytes: `put` writes `{"at":…`
 * first. Finding the oldest entries parsed every one of them whole, a
 * folder's worth of mail each, and did so at every write.
 */
function keptAt(raw: string | null): number {
  if (raw === null) return 0;
  const start = /^\{"at":(-?\d+(?:\.\d+)?)/.exec(raw);
  if (start) return Number(start[1]);
  try {
    return (JSON.parse(raw) as Entry<unknown>).at ?? 0;
  } catch {
    return 0;
  }
}

export class Cache {
  readonly #store: Store;
  readonly #prefix: string;

  constructor(store: Store, app: string) {
    this.#store = store;
    this.#prefix = `glass:${app}:`;
  }

  get<T>(key: string): { value: T; at: number } | null {
    const raw = this.#store.getItem(this.#prefix + key);
    if (raw === null) return null;
    try {
      const entry = JSON.parse(raw) as Entry<T>;
      return { value: entry.value, at: entry.at };
    } catch {
      return null;
    }
  }

  put<T>(key: string, value: T, now = Date.now()): void {
    const entry: Entry<T> = { at: now, value };
    try {
      this.#store.setItem(this.#prefix + key, JSON.stringify(entry));
    } catch {
      // Full: make room and try once more; failing that, just don't keep it.
      this.#trim(Math.floor(MAX_ENTRIES / 2));
      try {
        this.#store.setItem(this.#prefix + key, JSON.stringify(entry));
      } catch {
        return;
      }
    }
    this.#trim(MAX_ENTRIES);
  }

  /** This app's keys, as the store has them (no entry is read). */
  #mine(): string[] {
    const mine: string[] = [];
    for (let i = 0; i < this.#store.length; i++) {
      const key = this.#store.key(i);
      if (key?.startsWith(this.#prefix)) mine.push(key.slice(this.#prefix.length));
    }
    return mine;
  }

  /** Every key this app keeps, oldest first. */
  keys(): string[] {
    return this.#mine()
      .map((key) => ({ key, at: keptAt(this.#store.getItem(this.#prefix + key)) }))
      .sort((a, b) => a.at - b.at)
      .map((e) => e.key);
  }

  #trim(max: number): void {
    // Within the limit, as nearly every write is, nothing is read.
    if (this.#mine().length <= max) return;
    const keys = this.keys();
    for (const key of keys.slice(0, Math.max(0, keys.length - max))) {
      this.#store.removeItem(this.#prefix + key);
    }
  }
}
