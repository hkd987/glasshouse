// Asking for a meeting's media keys (ADR 0030 §6, ADR 0052).

import assert from "node:assert/strict";
import { test } from "node:test";

import { type Keys, askForKeys } from "../src/keys.ts";

const now = () => Promise.resolve();

/** A console that says a key is due `due` times, out of reach for the first `down`, then has one. */
function console(due: number, down = 0) {
  let asked = 0;
  return {
    get asked() {
      return asked;
    },
    ask: (): Promise<{ ok: boolean; data?: Keys }> => {
      asked += 1;
      if (asked <= down) return Promise.reject(new Error("offline"));
      if (asked <= down + due) return Promise.resolve({ ok: true, data: { media_keys: [{ id: "md-1", key: "a" }], due: true } });
      return Promise.resolve({ ok: true, data: { media_keys: [{ id: "md-2", key: "b" }, { id: "md-1", key: "a" }], due: false } });
    },
  };
}

test("adr0052 a page asks for keys for as long as a new one is due", async () => {
  // Found by specs/tla/MeetKeys.tla: the page gave up after six asks, so
  // when the new key took longer (the console out of reach for a while,
  // or each computer's new key refused as people came and went), the one
  // who left kept hearing everything said after.
  const held = new Set(["md-1"]);
  const take = (keys: Keys["media_keys"]) => {
    const current = keys[0];
    const fresh = current !== undefined && !held.has(current.id);
    for (const k of keys) held.add(k.id);
    return fresh;
  };
  const slow = console(10, 3);
  await askForKeys(slow.ask, take, () => true, now);
  assert.equal(slow.asked, 14);
  assert.ok(held.has("md-2"), "the new key arrived");
});

test("adr0052 a page stops asking when no key is due, or it left", async () => {
  const quiet = console(0);
  await askForKeys(quiet.ask, () => false, () => true, now);
  assert.equal(quiet.asked, 1);
  const gone = console(100);
  let live = 3;
  await askForKeys(gone.ask, () => false, () => (live -= 1) > 0, now);
  assert.equal(gone.asked, 2);
});
