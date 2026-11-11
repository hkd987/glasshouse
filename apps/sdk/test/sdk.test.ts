// The SDK's own tests: attribution lines, the offline cache, the bridge
// client's handling of refusals and outages, and manifests (UI-5, AU-10, DV-3).

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

import { parseAttribution, showAttribution } from "../src/attribution.ts";
import { ANSWER_LIMIT_MS, Bridge, BridgeError, Offline, type Transport } from "../src/bridge.ts";
import { Cache, MAX_ENTRIES, memoryStore } from "../src/cache.ts";
import { manifestProblems } from "../src/manifest.ts";

test("au10 attribution lines read by and for", () => {
  assert.deepEqual(parseAttribution("by claude-code for demo"), {
    text: "by claude-code for demo",
    by: "claude-code",
    for: "demo",
  });
  assert.equal(parseAttribution("by a for b for c")?.by, "a for b", "the user is the last name");
  for (const bad of ["", "claude-code for demo", "by  for demo", "by x", null, undefined]) {
    assert.equal(parseAttribution(bad), null, String(bad));
  }
  // What the user wrote through this app isn't flagged; an agent's always is.
  assert.equal(showAttribution(parseAttribution("by mail for demo"), "mail"), false);
  assert.equal(showAttribution(parseAttribution("by claude-code for demo"), "mail"), true);
  assert.equal(showAttribution(null, "mail"), false);
});

test("ui5 the cache keeps the newest entries per app", () => {
  const store = memoryStore();
  const mail = new Cache(store, "mail");
  const calendar = new Cache(store, "calendar");
  mail.put("a", { n: 1 }, 1);
  calendar.put("a", { n: 2 }, 2);
  assert.deepEqual(mail.get("a")?.value, { n: 1 });
  assert.deepEqual(calendar.get("a")?.value, { n: 2 }, "apps don't share a cache");
  for (let i = 0; i < MAX_ENTRIES + 5; i++) mail.put(`k${i}`, i, 10 + i);
  assert.equal(mail.keys().length, MAX_ENTRIES);
  assert.equal(mail.get("a"), null, "the oldest went first");
  assert.equal(mail.get(`k${MAX_ENTRIES + 4}`)?.value, MAX_ENTRIES + 4);
});

test("ui5 a write to the cache reads nothing it kept, until there is too much", () => {
  // Finding the oldest entries parsed every one whole (a folder of mail
  // each), at every write.
  const inner = memoryStore();
  let reads = 0;
  const store = {
    getItem: (key: string) => {
      reads += 1;
      return inner.getItem(key);
    },
    setItem: (key: string, value: string) => inner.setItem(key, value),
    removeItem: (key: string) => inner.removeItem(key),
    get length() {
      return inner.length;
    },
    key: (index: number) => inner.key(index),
  };
  const mail = new Cache(store, "mail");
  for (let i = 0; i < MAX_ENTRIES; i++) mail.put(`k${i}`, { big: "x".repeat(1000) }, 100 + i);
  assert.equal(reads, 0, "within the limit a write reads no entry");
  // One too many: the entries' times are read, and the oldest goes.
  mail.put("one-more", 1, 5000);
  assert.equal(mail.get("k0"), null);
  assert.equal(mail.get("k1")?.at, 101);
  assert.equal(mail.keys().length, MAX_ENTRIES);
  // An entry not written by put (hand-edited, or older) still has its time found.
  inner.setItem("glass:mail:odd", JSON.stringify({ value: 1, at: 1 }));
  assert.equal(mail.keys()[0], "odd");
  inner.setItem("glass:mail:broken", "{");
  assert.equal(mail.keys()[0], "broken", "what can't be read counts as oldest");
});

/** A bridge that answers from a script, or is down. */
function fake(
  answers: Record<string, { status: number; body: unknown }>,
): Transport & { down: boolean; calls: string[]; limits: Record<string, number | undefined> } {
  const transport = {
    down: false,
    calls: [] as string[],
    /** How long each kind of request was given to be answered. */
    limits: {} as Record<string, number | undefined>,
    async post(endpoint: string, body: unknown, limitMs?: number) {
      transport.calls.push(`${endpoint} ${JSON.stringify(body)}`);
      const wait = (body as { wait_secs?: number } | null)?.wait_secs;
      transport.limits[endpoint === "call" || wait === undefined ? endpoint : `${endpoint} waiting`] = limitMs;
      if (transport.down) throw new TypeError("Failed to fetch");
      const key = endpoint === "call" ? `call ${(body as { action: string }).action}` : endpoint;
      return answers[key] ?? { status: 404, body: { error: "no" } };
    },
  };
  return transport;
}

const session = {
  app: "mail",
  name: "Mail",
  version: "0.1.0",
  user: "demo",
  actions: [
    { name: "mail.list", title: "Read the mailboxes", tier: "auto", side_effect: "none" },
    { name: "mail.send", title: "Send emails", tier: "ask", side_effect: "irreversible" },
  ],
  permissions: ["offline"],
};

test("ui5 the client knows each action's tier and passes refusals on as results", async () => {
  const transport = fake({
    session: { status: 200, body: session },
    "call mail.send": { status: 200, body: { ok: false, text: "Declined on the desktop, so this wasn't done: Send email.", data: null } },
  });
  const bridge = new Bridge({ app: "mail", transport, store: memoryStore() });
  assert.equal((await bridge.open()).user, "demo");
  assert.equal(bridge.asks("mail.send"), true);
  assert.equal(bridge.asks("mail.list"), false);
  assert.equal(bridge.tier("files.delete"), undefined, "undeclared");
  const sent = await bridge.call("mail.send", { messages: [] });
  assert.equal(sent.ok, false);
  assert.match(sent.text, /^Declined/);
  await assert.rejects(bridge.call("mail.draft"), BridgeError);
});

test("ad3 an open app's tiers follow the policy IT publishes", async () => {
  const stricter = session.actions.map((a) => (a.name === "mail.list" ? { ...a, tier: "never" } : a));
  const transport = fake({
    session: { status: 200, body: session },
    status: { status: 200, body: { live: true, waiting: null, cursor: 3, changed: [], actions: stricter } },
  });
  const bridge = new Bridge({ app: "mail", transport, store: memoryStore() });
  await bridge.open();
  assert.equal(bridge.tier("mail.list"), "auto");
  await bridge.status({ waitSecs: 25, since: 2 });
  assert.equal(bridge.tier("mail.list"), "never");
  assert.equal(bridge.asks("mail.send"), true);
});

test("ui5 reads show what was loaded when the system can't be reached", async () => {
  const inbox = { ok: true, text: "1 message in Inbox", data: { messages: [{ id: "inbox/1.eml" }] } };
  const transport = fake({
    session: { status: 200, body: session },
    "call mail.list": { status: 200, body: inbox },
  });
  const store = memoryStore();
  const bridge = new Bridge({ app: "mail", transport, store });
  await bridge.open();
  const live = await bridge.read("mail.list", { folder: "inbox" });
  assert.equal(live.offline, false);

  transport.down = true;
  const again = new Bridge({ app: "mail", transport, store });
  assert.equal((await again.open()).user, "demo", "the last session seen");
  const cached = await again.read("mail.list", { folder: "inbox" });
  assert.equal(cached.offline, true);
  assert.deepEqual(cached.result, inbox);
  assert.equal(again.offline, true);
  // Nothing loaded before: nothing to show. And sends never queue.
  await assert.rejects(again.read("mail.list", { folder: "sent" }), Offline);
  await assert.rejects(again.call("mail.send", {}), Offline);
});

const DAY = 24 * 60 * 60 * 1000;

test("ui5 what an app kept is forgotten after the organization's days, if it set any", () => {
  const store = memoryStore();
  const mail = new Cache(store, "mail");
  const calendar = new Cache(store, "calendar");
  mail.put("old", 1, 0);
  mail.put("new", 2, 6 * DAY);
  calendar.put("old", 3, 0);
  // No rule: kept, however old.
  assert.equal(mail.get("old", 400 * DAY)?.value, 1);
  // Seven days: what is older goes as the rule arrives. Another app's
  // entries are its own to forget, when it opens.
  mail.keepFor(7, 8 * DAY);
  assert.deepEqual(mail.keys(), ["new"]);
  assert.equal(mail.get("old", 8 * DAY), null);
  assert.equal(calendar.get("old", 8 * DAY)?.value, 3);
  // And at a read later on, with the page open all the while.
  assert.equal(mail.get("new", 13 * DAY)?.value, 2);
  assert.equal(mail.get("new", 14 * DAY), null);
  assert.deepEqual(mail.keys(), [], "an entry past the rule is removed, not only hidden");
  // The rule lifted: kept until replaced, as before.
  mail.put("again", 4, 14 * DAY);
  mail.keepFor(null, 400 * DAY);
  assert.equal(mail.get("again", 400 * DAY)?.value, 4);
});

test("ui5 offline, an app shows what it kept only within the organization's days", async () => {
  const inbox = { ok: true, text: "1 message in Inbox", data: { messages: [{ id: "inbox/1.eml" }] } };
  const store = memoryStore();
  let now = 0;
  const opened = (rule: object) => {
    const transport = fake({
      session: { status: 200, body: { ...session, ...rule } },
      "call mail.list": { status: 200, body: inbox },
    });
    return { transport, bridge: () => new Bridge({ app: "mail", transport, store, now: () => now }) };
  };

  // The organization keeps seven days. Loaded on day 0, then no connection.
  const week = opened({ offline_days: 7 });
  const first = week.bridge();
  assert.equal((await first.open()).offline_days, 7);
  await first.read("mail.list", { folder: "inbox" });
  week.transport.down = true;
  // Day 3: the app opens on the last session seen, and shows the inbox.
  now = 3 * DAY;
  const soon = week.bridge();
  await soon.open();
  assert.equal((await soon.read("mail.list", { folder: "inbox" })).offline, true);
  // Day 8: neither. The rule came with the session, so it holds with no
  // connection to ask over, and what was kept is gone from the device.
  now = 8 * DAY;
  await assert.rejects(week.bridge().open(), Offline);
  assert.deepEqual(new Cache(store, "mail").keys(), []);

  // With no rule (the default), a year on it still opens and shows it.
  const always = opened({});
  now = 10 * DAY;
  const live = always.bridge();
  assert.equal((await live.open()).offline_days, undefined);
  await live.read("mail.list", { folder: "inbox" });
  always.transport.down = true;
  now = 375 * DAY;
  const later = always.bridge();
  await later.open();
  assert.equal((await later.read("mail.list", { folder: "inbox" })).offline, true);
});

test("ui5 a wait for news ends by itself; a call that may wait for a person doesn't", async () => {
  const transport = fake({
    session: { status: 200, body: session },
    status: { status: 200, body: { open: true, pending: [], cursor: 7 } },
    live: { status: 200, body: { updates: [], cursor: 0 } },
    "call mail.send": { status: 200, body: { ok: true, text: "Sent", data: null } },
  });
  const bridge = new Bridge({ app: "mail", transport, store: memoryStore() });
  await bridge.open();
  await bridge.status();
  await bridge.status({ waitSecs: 25, since: 7 });
  await bridge.live({ op: "pull", doc: "d", since: 0, wait_secs: 20 });
  await bridge.live({ op: "push", doc: "d", update: "AA==" });
  await bridge.call("mail.send", { to: ["ana@example.com"] });
  assert.deepEqual(transport.limits, {
    session: ANSWER_LIMIT_MS,
    status: ANSWER_LIMIT_MS,
    // As long as it asked the bridge to wait, and then as long as any answer.
    "status waiting": 25_000 + ANSWER_LIMIT_MS,
    "live waiting": 20_000 + ANSWER_LIMIT_MS,
    live: ANSWER_LIMIT_MS,
    // Approval can take minutes: nothing gives up on it.
    call: undefined,
  });

  // Given up on, a wait is the system not being reached: the page says so
  // and asks again, where it used to wait for good and hear nothing.
  const silent: Transport = {
    post: (_endpoint, _body, limitMs) =>
      new Promise((_, reject) => {
        if (limitMs !== undefined) setTimeout(() => reject(new DOMException("timed out", "TimeoutError")), 1);
      }),
  };
  const waiting = new Bridge({ app: "mail", transport: silent, store: memoryStore() });
  await assert.rejects(waiting.status({ waitSecs: 25, since: 7 }), Offline);
  assert.equal(waiting.offline, true);
});

test("ui5 a bridge that answers 503 is offline too", async () => {
  const bridge = new Bridge({
    app: "mail",
    transport: fake({ session: { status: 503, body: { error: "down" } } }),
    store: memoryStore(),
  });
  await assert.rejects(bridge.open(), Offline);
});

test("dv3 the first-party manifests check out, and overreach is caught", () => {
  for (const app of ["mail", "calendar"]) {
    const manifest = JSON.parse(readFileSync(new URL(`../../${app}/manifest.json`, import.meta.url), "utf8"));
    assert.deepEqual(manifestProblems(manifest), [], app);
    assert.equal(manifest.id, app);
  }
  const base = { id: "notes", name: "Notes", version: "1", actions: ["files.read"], labels: ["internal"] };
  assert.deepEqual(manifestProblems(base), []);
  const problems = (extra: object) => manifestProblems({ ...base, ...extra }).join("; ");
  assert.match(problems({ actions: ["files.*"] }), /not an action name/);
  assert.match(problems({ actions: ["files.read", "files.read"] }), /twice/);
  assert.match(problems({ labels: ["restricted"] }), /restricted/);
  assert.match(problems({ network: ["example.com:443"] }), /network/);
  assert.match(problems({ name: "Notes\nApproved" }), /one line/);
  assert.match(problems({ tiers: { "files.read": "auto" } }), /unknown field tiers/);
  assert.match(problems({ id: "Notes App" }), /slug/);
});

test("cf21 only a meeting app has the camera and microphone; Chat never does", () => {
  const chat = JSON.parse(readFileSync(new URL("../../chat/manifest.json", import.meta.url), "utf8"));
  assert.ok(!(chat.permissions ?? []).includes("media"));
  assert.match(manifestProblems({ ...chat, permissions: ["media"] }).join("; "), /only an app that declares meet\.join/);
  const meet = { id: "meet", name: "Meet", version: "0.1.0", actions: ["meet.list", "meet.join"], permissions: ["media"], labels: ["internal"] };
  assert.deepEqual(manifestProblems(meet), []);
  // ADR 0045 T2: speech, only for an app that adds to transcripts.
  assert.match(manifestProblems({ ...meet, permissions: ["media", "speech"] }).join("; "), /only an app that declares meet\.transcribe/);
  assert.deepEqual(manifestProblems({ ...meet, actions: [...meet.actions, "meet.transcribe"], permissions: ["media", "speech"] }), []);
});

test("nv3 a page hears of its app's changes, and asks a system that is down less and less often", async () => {
  const { pauseAfter, watch } = await import("../src/live.ts");
  // 1 s doubling to half a minute, give or take a fifth.
  assert.deepEqual(
    [1, 2, 3, 6, 20].map((n) => pauseAfter(n, 0.5)),
    [1000, 2000, 4000, 30_000, 30_000],
  );
  assert.equal(pauseAfter(1, 0), 800);
  assert.equal(pauseAfter(1, 1), 1200);

  // A system that is down twice, then has news of another app, then of ours.
  const answers: (() => { status: number; body: unknown })[] = [
    () => {
      throw new TypeError("Failed to fetch");
    },
    () => {
      throw new TypeError("Failed to fetch");
    },
    () => ({ status: 200, body: { open: true, pending: [], cursor: 8, changed: ["mail"] } }),
    () => ({ status: 200, body: { open: true, pending: [], cursor: 9, changed: ["calendar"] } }),
  ];
  const asked: number[] = [];
  const waits: number[] = [];
  let done: () => void = () => {};
  const finished = new Promise<void>((resolve) => (done = resolve));
  const transport: Transport = {
    async post(_endpoint, body) {
      asked.push((body as { since: number }).since);
      waits.push((body as { wait_secs: number }).wait_secs);
      const next = answers.shift();
      if (!next) return new Promise(() => {}); // nothing more: the wait stays open
      return next();
    },
  };
  const bridge = new Bridge({ app: "calendar", transport, store: memoryStore() });
  const heard: string[] = [];
  const pauses: number[] = [];
  const watching = watch(
    bridge,
    "calendar",
    7,
    () => {
      heard.push("changed");
      done();
    },
    {
      onOffline: (offline) => heard.push(offline ? "offline" : "back"),
      onAnswer: () => heard.push("answered"),
      pauseMs: (failures) => {
        pauses.push(failures);
        return 1;
      },
    },
  );
  await finished;
  watching.stop();
  // Told once it was down and once it was back; our change, not Mail's;
  // and of each answer, a change of ours or not (stopped before the last
  // one's).
  assert.deepEqual(heard, ["offline", "back", "answered", "changed"]);
  // The pause grew with each failure in a row.
  assert.deepEqual(pauses, [1, 2]);
  // Each wait from the cursor the last answer gave.
  assert.deepEqual(asked.slice(0, 4), [7, 7, 7, 8]);
  // An ask after a failure doesn't wait for news, so "back" is said when
  // the system answers and not 25 s on; then the waits are long again.
  assert.deepEqual(waits.slice(0, 4), [25, 0, 0, 25]);
});

