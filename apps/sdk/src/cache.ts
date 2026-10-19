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

  /** Every key this app keeps, oldest first. */
  keys(): string[] {
    const mine: { key: string; at: number }[] = [];
    for (let i = 0; i < this.#store.length; i++) {
      const key = this.#store.key(i);
      if (key?.startsWith(this.#prefix)) {
        const short = key.slice(this.#prefix.length);
        mine.push({ key: short, at: this.get(short)?.at ?? 0 });
      }
    }
    return mine.sort((a, b) => a.at - b.at).map((e) => e.key);
  }

  #trim(max: number): void {
    const keys = this.keys();
    for (const key of keys.slice(0, Math.max(0, keys.length - max))) {
      this.#store.removeItem(this.#prefix + key);
    }
  }
}
