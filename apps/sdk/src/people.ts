// Addressing people (AD-1): what's being typed in an address field, the
// suggestions for it, and putting a chosen address in. No DOM here, so it
// is tested on its own; `address-field.ts` draws it.

/** Someone to address, as `people.search` returns them or recent mail shows them. */
export interface Person {
  name: string;
  email: string;
  title?: string;
  department?: string;
}

/** The most suggestions an address field shows. */
export const SUGGESTIONS = 8;

const SEPARATOR = /[,;\s]/;

/** The address being typed at `caret`: from the separator before it to the one after. */
export function currentToken(text: string, caret: number): { start: number; end: number; token: string } {
  let start = caret;
  while (start > 0 && !SEPARATOR.test(text[start - 1] ?? "")) start--;
  let end = caret;
  while (end < text.length && !SEPARATOR.test(text[end] ?? "")) end++;
  return { start, end, token: text.slice(start, end) };
}

/** `text` with the token at [start, end) replaced by `email` and a separator, and where the caret goes. */
export function insertAddress(text: string, start: number, end: number, email: string): { text: string; caret: number } {
  const before = text.slice(0, start);
  const after = text.slice(end).replace(/^[,;\s]*/, "");
  const inserted = `${email}, `;
  return { text: before + inserted + after, caret: before.length + inserted.length };
}

const ADDRESS = /^[^\s@<>,;]+@[^\s@<>,;]+\.[^\s@<>,;]+$/;

/** Splits what someone typed in an address field into addresses, and what isn't one. */
export function parseAddresses(text: string): { addresses: string[]; bad: string[] } {
  const parts = text
    .split(/[,;\s]+/)
    .map((p) => p.trim())
    .filter(Boolean);
  return {
    addresses: parts.filter((p) => ADDRESS.test(p)),
    bad: parts.filter((p) => !ADDRESS.test(p)),
  };
}

/** The addresses already in `text`, lower case. */
export function addressesIn(text: string): string[] {
  return parseAddresses(text).addresses.map((a) => a.toLowerCase());
}

/** Whether `person` is what `query` is looking for: every word starts a word of their name or address. */
export function matches(person: Person, query: string): boolean {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (!words.length) return false;
  const tokens = [
    ...person.name.toLowerCase().split(/\s+/),
    ...person.email.toLowerCase().split(/[@._+-]/),
    person.email.toLowerCase(),
  ].filter(Boolean);
  return words.every((w) => tokens.some((t) => t.startsWith(w)));
}

/**
 * The suggestions to show: the org's directory first, then people from
 * recent mail it didn't find, never anyone already addressed, each address
 * once, at most `limit`.
 */
export function mergeSuggestions(
  directory: readonly Person[],
  recent: readonly Person[],
  already: readonly string[],
  query: string,
  limit = SUGGESTIONS,
): Person[] {
  const seen = new Set(already.map((a) => a.toLowerCase()));
  const out: Person[] = [];
  for (const person of [...directory, ...recent.filter((p) => matches(p, query))]) {
    const key = person.email.toLowerCase();
    if (!person.email || seen.has(key)) continue;
    seen.add(key);
    out.push(person);
    if (out.length >= limit) break;
  }
  return out;
}

/**
 * A lookup for a longer query can be answered from a shorter one's
 * results, when those were all there was (fewer than the limit): returns
 * them narrowed, or null when the directory has to be asked.
 */
export function narrowed(
  cache: ReadonlyMap<string, readonly Person[]>,
  query: string,
  limit = SUGGESTIONS,
): Person[] | null {
  const q = query.toLowerCase();
  for (let n = q.length - 1; n >= 2; n--) {
    const earlier = cache.get(q.slice(0, n));
    if (earlier && earlier.length < limit) return earlier.filter((p) => matches(p, q));
  }
  return null;
}

/** The highlighted suggestion after Up or Down: wraps round; -1 is none. */
export function stepHighlight(current: number, count: number, key: "ArrowDown" | "ArrowUp"): number {
  if (count === 0) return -1;
  if (key === "ArrowDown") return current + 1 >= count ? 0 : current + 1;
  return current - 1 < 0 ? count - 1 : current - 1;
}

/** People from mail the person has: who wrote to them and who they wrote to. */
export function recentPeople(addresses: readonly string[]): Person[] {
  const out: Person[] = [];
  const seen = new Set<string>();
  for (const raw of addresses) {
    const m = /^\s*"?([^"<]*?)"?\s*<([^>]+)>\s*$/.exec(raw);
    const email = (m?.[2] ?? raw).trim();
    const name = (m?.[1] ?? "").trim();
    const key = email.toLowerCase();
    if (!email.includes("@") || seen.has(key)) continue;
    seen.add(key);
    out.push({ name: name || email, email });
  }
  return out;
}
