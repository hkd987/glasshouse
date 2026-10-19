// Meetings' media path (CF-21, ADR 0030 §6, slice M1). Two people join a
// meeting through the relay and see and hear each other; someone the
// relay admits but who holds another key decodes nothing; joining takes
// under two seconds (UI-7); the page keeps no key anywhere.
//
// The test plays glassd and the console: it answers the page's bridge
// calls and hands each offer to the relay's private API. Each page's
// peer connection is found through an init script, never through the app.

import { type ChildProcess, spawn } from "node:child_process";
import { readFile } from "node:fs/promises";
import { dirname, extname, join } from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";
import { type Browser, expect, type Page, test } from "@playwright/test";

const REPO = join(dirname(fileURLToPath(import.meta.url)), "../..");
const TARGET = process.env.CARGO_TARGET_DIR ?? join(REPO, "target");
const DIST = join(REPO, "apps/meet/dist");
const ORIGIN = "http://meet.localhost:7398";
const JOIN_BUDGET_MS = 2000;
const SHOTS = join(REPO, "apps/e2e/results/meet-shots");
const TYPES: Record<string, string> = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css" };

let relay: ChildProcess;
let api = "";

test.beforeAll(async () => {
  relay = spawn(join(TARGET, "debug/glass-sfu"), ["--udp", "127.0.0.1:0", "--public", "127.0.0.1", "--api", "127.0.0.1:0"], {
    stdio: ["ignore", "pipe", "inherit"],
  });
  const lines = createInterface({ input: relay.stdout! });
  api = await new Promise<string>((ready, failed) => {
    relay.once("exit", (code) => failed(new Error(`glass-sfu exited (${code})`)));
    lines.on("line", (line) => {
      const found = /^glass-sfu ready udp=\S+ api=(\S+)$/.exec(line);
      if (found?.[1]) ready(`http://${found[1]}`);
    });
  });
});

test.afterAll(() => {
  relay?.kill();
});

const base64 = (bytes: Uint8Array) => Buffer.from(bytes).toString("base64");

interface Seat {
  room: string;
  participant: number;
  mediaKey: Uint8Array;
}

/** One person's Meet page, with the test standing in for glassd. */
async function person(browser: Browser, seat: Seat): Promise<Page> {
  const context = await browser.newContext();
  const page = await context.newPage();
  page.on("pageerror", (e) => console.error(`page error: ${e.message}`));
  await page.addInitScript(() => {
    const seen: RTCPeerConnection[] = [];
    (window as unknown as { __pcs: RTCPeerConnection[] }).__pcs = seen;
    const Native = window.RTCPeerConnection;
    window.RTCPeerConnection = class extends Native {
      constructor(config?: RTCConfiguration) {
        super(config);
        seen.push(this);
      }
    } as typeof RTCPeerConnection;
  });
  await page.route(`${ORIGIN}/**`, async (route) => {
    const url = new URL(route.request().url());
    if (url.pathname === "/_bridge/session") {
      return route.fulfill({
        json: {
          app: "meet",
          name: "Meet",
          version: "0.1.0",
          user: `person-${seat.participant}`,
          permissions: ["media"],
          actions: ["meet.list", "meet.join", "meet.keys", "meet.leave"].map((name) => ({ name, title: name, tier: "auto", side_effect: "none" })),
        },
      });
    }
    if (url.pathname === "/_bridge/call") {
      const { action, input } = route.request().postDataJSON() as { action: string; input: { offer?: string } };
      if (action === "meet.join") {
        const response = await fetch(`${api}/rooms/${seat.room}/participants`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ participant: seat.participant, offer: input.offer }),
        });
        const body = (await response.json()) as { answer?: string; error?: string };
        if (!response.ok || !body.answer) return route.fulfill({ json: { ok: false, text: body.error ?? "refused", data: null } });
        return route.fulfill({
          json: { ok: true, text: "Joined", data: { answer: body.answer, media_key: base64(seat.mediaKey), participant: seat.participant } },
        });
      }
      if (action === "meet.leave") {
        await fetch(`${api}/rooms/${seat.room}/participants/${seat.participant}`, { method: "DELETE" });
        return route.fulfill({ json: { ok: true, text: "Left", data: null } });
      }
      return route.fulfill({ json: { ok: false, text: `not in this test: ${action}`, data: null } });
    }
    const file = url.pathname === "/" ? "index.html" : url.pathname.slice(1);
    try {
      return route.fulfill({ body: await readFile(join(DIST, file)), contentType: TYPES[extname(file)] ?? "application/octet-stream" });
    } catch {
      return route.fulfill({ status: 404, body: "" });
    }
  });
  await page.goto(`${ORIGIN}/#${seat.room}`);
  await page.waitForSelector("html[data-ready=true]");
  return page;
}

interface Received {
  framesDecoded: number;
  audioSamples: number;
}

/** What the page has decoded from the other person so far. */
async function received(page: Page): Promise<Received> {
  return page.evaluate(async () => {
    const out = { framesDecoded: 0, audioSamples: 0 };
    for (const pc of (window as unknown as { __pcs: RTCPeerConnection[] }).__pcs) {
      (await pc.getStats()).forEach((s) => {
        if (s.type !== "inbound-rtp") return;
        if (s.kind === "video") out.framesDecoded += s.framesDecoded ?? 0;
        if (s.kind === "audio") out.audioSamples += (s.totalSamplesReceived ?? 0) - (s.concealedSamples ?? 0);
      });
    }
    return out;
  });
}

const key = () => crypto.getRandomValues(new Uint8Array(16));

test("cf21 ui7 two people see and hear each other through the relay, joining in under two seconds", async ({ browser }) => {
  const mediaKey = key();
  const alice = await person(browser, { room: "standup", participant: 0, mediaKey });
  const bob = await person(browser, { room: "standup", participant: 1, mediaKey });
  await alice.click("[data-test=join]");
  await expect(alice.locator("[data-test=waiting]")).toBeVisible();

  const started = Date.now();
  await bob.click("[data-test=join]");
  await expect.poll(async () => (await received(bob)).framesDecoded, { timeout: 10_000, intervals: [50] }).toBeGreaterThan(0);
  const joinMs = Date.now() - started;
  console.log(`meet: second person sees the first ${joinMs} ms after Join`);
  expect(joinMs).toBeLessThan(JOIN_BUDGET_MS);

  for (const page of [alice, bob]) {
    await expect.poll(async () => (await received(page)).framesDecoded, { timeout: 10_000 }).toBeGreaterThan(30);
    await expect.poll(async () => (await received(page)).audioSamples, { timeout: 10_000 }).toBeGreaterThan(48_000);
    await expect(page.locator("video[data-test=other]")).toBeVisible();
  }
  await alice.screenshot({ path: join(SHOTS, "two-people-alice.png") });
  await bob.screenshot({ path: join(SHOTS, "two-people-bob.png") });

  // The page keeps no key: nothing in storage, and no database.
  for (const page of [alice, bob]) {
    const kept = await page.evaluate(async () => ({
      local: JSON.stringify({ ...localStorage }),
      session: JSON.stringify({ ...sessionStorage }),
      databases: (await indexedDB.databases()).length,
    }));
    expect(kept.local).not.toContain(base64(mediaKey));
    expect(kept.session).not.toContain(base64(mediaKey));
    expect(kept.databases).toBe(0);
  }

  await bob.click("[data-test=leave]");
  await expect(bob.locator("[data-test=message]")).toContainText("You left");
  await expect(alice.locator("[data-test=waiting]")).toBeVisible();
});

test("cf21 someone the relay admits without the meeting's key decodes nothing", async ({ browser }) => {
  const alice = await person(browser, { room: "board", participant: 0, mediaKey: key() });
  const eve = await person(browser, { room: "board", participant: 1, mediaKey: key() });
  await alice.click("[data-test=join]");
  await eve.click("[data-test=join]");
  // Packets flow both ways, so the relay forwards them...
  await expect
    .poll(async () => eve.evaluate(async () => {
      let packets = 0;
      for (const pc of (window as unknown as { __pcs: RTCPeerConnection[] }).__pcs) {
        (await pc.getStats()).forEach((s) => {
          if (s.type === "inbound-rtp" && s.kind === "video") packets += s.packetsReceived ?? 0;
        });
      }
      return packets;
    }), { timeout: 10_000 })
    .toBeGreaterThan(50);
  // ...but no frame opens with another key, so none is decoded.
  await alice.waitForTimeout(1000);
  expect((await received(eve)).framesDecoded).toBe(0);
  expect((await received(alice)).framesDecoded).toBe(0);
  await eve.screenshot({ path: join(SHOTS, "wrong-key.png") });
});
