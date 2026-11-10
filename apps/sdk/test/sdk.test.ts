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
