// Asking for a meeting's media keys (ADR 0030 §6): after someone came or
// went, a new key may be due, and the first computer to ask makes it.

/** `meet.keys`: the session's media keys, current first. */
export interface Keys {
  media_keys: { id: string; key: string }[];
  /** A new key is still due; ask again shortly. */
  due?: boolean;
}

/** How the page asks: `meet.keys` (throws when glassd can't be reached). */
export type Ask = () => Promise<{ ok: boolean; data?: Keys }>;

/** The longest wait between asks. */
export const MAX_WAIT_MS = 8_000;

/**
 * Asks, backing off to every eight seconds, until there is a new key or
 * none is due: for as long as one is, since until then whoever left can
 * open what is said (ADR 0052). `take` hands the answer's keys to the
 * worker and says whether the current one is new; `live` says whether
 * this is still the page's newest ask and it is still in the call.
 */
export async function askForKeys(
  ask: Ask,
  take: (keys: Keys["media_keys"]) => boolean,
  live: () => boolean,
  sleep: (ms: number) => Promise<void> = (ms) => new Promise((done) => setTimeout(done, ms)),
): Promise<void> {
  for (let wait = 250; ; wait = Math.min(wait * 2, MAX_WAIT_MS)) {
    await sleep(wait);
    if (!live()) return;
    try {
      const result = await ask();
      if (result.ok && (take(result.data?.media_keys ?? []) || result.data?.due === false)) return;
    } catch {
      // Not reachable right now: try again.
    }
  }
}
