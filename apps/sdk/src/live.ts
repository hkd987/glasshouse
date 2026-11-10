// Hearing of changes to the person's data while a page is open (NV-3, ADR
// 0030, 0051): the wait on `app.status`, written once.
//
// Each app had its own copy of this loop, with a fixed three-second pause
// after a failure: against a system that is down, every open window asked
// again every three seconds for as long as it was. Here the pause grows to
// half a minute, with some chance in it so windows don't ask together.
//
// The rules a page has to keep (ADR 0051) are in how this is called: take
// the cursor with [`firstCursor`] BEFORE the first load, so a change that
// lands between the two is news; and read again in `changed` every time,
// since the wait names only that something changed.

import type { Bridge } from "./bridge.ts";

/** Stops a [`watch`]. */
export interface Watching {
  stop(): void;
}

export interface WatchOptions {
  /** How long each wait asks the system to hold (25 s at most). */
  waitSecs?: number;
  /** Told when the system stops being reached, and when it is again. */
  onOffline?: (offline: boolean) => void;
  /**
   * Told each time the system answers a wait, after `changed` if there
   * was a change: for a page that marks itself offline elsewhere too (a
   * load that failed) and takes any answer as being back.
   */
  onAnswer?: () => void;
  /** For tests: the pause after the nth failure in a row, in ms. */
  pauseMs?: (failures: number) => number;
}

/** The cursor to watch from: taken before the page's first load. 0 if the system can't be reached. */
export async function firstCursor(bridge: Bridge): Promise<number> {
  return bridge.status().then(
    (status) => status.cursor,
    () => 0,
  );
}

/** The pause after `failures` in a row: 1 s doubling to 30 s, give or take a fifth. */
export function pauseAfter(failures: number, chance: number = Math.random()): number {
  const base = Math.min(30_000, 1000 * 2 ** Math.max(0, failures - 1));
  return Math.round(base * (0.8 + 0.4 * chance));
}

/**
 * Waits for this app's data to change after `cursor` and calls `changed`
 * each time it has, until stopped. `changed` is awaited: a change that
 * comes while it runs is the next wait's news, never lost.
 */
export function watch(bridge: Bridge, app: string, cursor: number, changed: () => Promise<void> | void, options: WatchOptions = {}): Watching {
  let stopped = false;
  let failures = 0;
  const waitSecs = options.waitSecs ?? 25;
  const pause = options.pauseMs ?? pauseAfter;
  void (async () => {
    while (!stopped) {
      try {
        const status = await bridge.status({ waitSecs, since: cursor });
        cursor = status.cursor;
        if (failures > 0) options.onOffline?.(false);
        failures = 0;
        if (!stopped && status.changed.includes(app)) await changed();
        if (!stopped) options.onAnswer?.();
      } catch {
        failures += 1;
        if (failures === 1) options.onOffline?.(true);
        await new Promise((resolve) => setTimeout(resolve, pause(failures)));
      }
    }
  })();
  return {
    stop() {
      stopped = true;
    },
  };
}
