// Live updates (ADR 0066): a long poll of /api/v1/events, what an answer
// means, and how the loop backs off when the front can't be reached.

import assert from "node:assert/strict";
import { test } from "node:test";

import type { Events } from "../src/api.ts";
import { type Change, EventLoop, type Front, MAX_WAIT_MS, MIN_GAP_MS, backoff, whatChanged } from "../src/events.ts";

test("adr0066 an answer names what to refetch", () => {
  // 25 s with nothing: the same cursor back.
  assert.deepEqual(whatChanged(4, { cursor: 4, agents: [] }), { kind: "nothing" });
  assert.deepEqual(whatChanged(4, { cursor: 6, agents: ["reconciler", "reconciler", "researcher"] }), { kind: "agents", ids: ["reconciler", "researcher"] });
  // An empty list with a new cursor is everything (the front fell behind).
  assert.deepEqual(whatChanged(4, { cursor: 9, agents: [] }), { kind: "all" });
  // The front started over: its cursor went back.
  assert.deepEqual(whatChanged(40, { cursor: 2, agents: ["reconciler"] }), { kind: "all" });
});

test("adr0066 the backoff doubles from 1 s to 30 s, a little less at random", () => {
  const top = () => 1;
  assert.deepEqual(
    [0, 1, 2, 3, 4, 5, 6, 7, 20].map((n) => backoff(n, top)),
    [0, 1000, 2000, 4000, 8000, 16_000, 30_000, 30_000, 30_000],
  );
  assert.equal(backoff(1, () => 0), 750);
  assert.equal(backoff(30, () => 0), MAX_WAIT_MS * 0.75);
  for (let i = 0; i < 50; i++) {
    const wait = backoff(3);
    assert.ok(wait >= 3000 && wait <= 4000, String(wait));
  }
});

type Step = Events | Error;

/** A front that answers from a script, and a clock that moves 10 s per
 * poll unless a step says it answered at once. */
function fake(steps: Step[], quick = new Set<number>()) {
  const log: string[] = [];
  const changes: Change[] = [];
  let time = 0;
  let polls = 0;
  let loop: EventLoop | null = null;
  const front: Front = {
    async poll(since) {
      const step = steps[polls];
      log.push(`poll ${since}`);
      if (!quick.has(polls)) time += 10_000;
      polls += 1;
      if (!step) {
        loop?.stop();
        throw new Error("done");
      }
      if (step instanceof Error) throw step;
      return step;
    },
    changed(change) {
      changes.push(change);
      log.push(`changed ${change.kind === "agents" ? change.ids.join(",") : change.kind}`);
    },
    online(yes) {
      log.push(yes ? "online" : "offline");
    },
    async sleep(ms) {
      log.push(`sleep ${ms}`);
      time += ms;
    },
    now: () => time,
    random: () => 1,
  };
  return {
    front,
    log,
    changes,
    start(cursor = 0) {
      loop = new EventLoop(front, cursor);
      return loop.run().then(() => loop as EventLoop);
    },
  };
}

test("adr0066 the loop refetches what changed and polls again from the new cursor", async () => {
  const f = fake([{ cursor: 3, agents: ["reconciler"] }, { cursor: 3, agents: [] }, { cursor: 5, agents: [] }]);
  const loop = await f.start(0);
  assert.deepEqual(f.log, ["poll 0", "changed reconciler", "poll 3", "poll 3", "changed all", "poll 5"]);
  assert.equal(loop.cursor, 5);
  assert.equal(loop.running, false);
});

test("adr0066 when the front can't be reached it backs off, says so after two misses, and refetches all when back", async () => {
  const down = new Error("unreachable");
  const f = fake([down, down, down, { cursor: 8, agents: ["researcher"] }, { cursor: 8, agents: [] }]);
  await f.start(2);
  assert.deepEqual(f.log, [
    "poll 2",
    "sleep 1000",
    "poll 2",
    "offline",
    "sleep 2000",
    "poll 2",
    "sleep 4000",
    "poll 2",
    "online",
    // Something may have been missed while it was away.
    "changed all",
    "poll 8",
    "poll 8",
  ]);
});

test("adr0066 a front that answers at once with nothing isn't asked again sooner than a second", async () => {
  const f = fake([{ cursor: 1, agents: [] }, { cursor: 1, agents: [] }], new Set([1]));
  await f.start(1);
  assert.deepEqual(f.log, ["poll 1", "poll 1", `sleep ${MIN_GAP_MS}`, "poll 1"]);
});

test("adr0066 a refetch that fails is asked again from the same cursor", async () => {
  const f = fake([{ cursor: 4, agents: ["reconciler"] }, { cursor: 4, agents: ["reconciler"] }]);
  let first = true;
  const changed = f.front.changed;
  f.front.changed = (change) => {
    if (first) {
      first = false;
      throw new Error("the refetch failed");
    }
    return changed(change);
  };
  const loop = await f.start(0);
  assert.deepEqual(f.log, ["poll 0", "sleep 1000", "poll 0", "changed all", "poll 4"]);
  assert.equal(loop.cursor, 4);
});

test("adr0066 signed out ends the loop", async () => {
  class SignedOut extends Error {}
  const f = fake([new SignedOut("sign in"), { cursor: 1, agents: [] }]);
  f.front.fatal = (error) => error instanceof SignedOut;
  await f.start(0);
  assert.deepEqual(f.log, ["poll 0"]);
});
