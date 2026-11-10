// Meetings' media path (CF-21, ADR 0030 §6, slices M1 and M2). People join
// a meeting through the relay and see and hear each other; someone the
// relay admits but who holds another key decodes nothing; three people
// each decode the other two, the third joining later; a shared screen
// reaches the others and is shown large; after someone leaves, the others
// switch to the new media key and still see each other; joining takes
// under two seconds and the page stays within its memory (UI-7); the page
// keeps no key anywhere. In a transcribed meeting (ADR 0045 T2), what the
// engine's microphone plays (a recording) becomes lines of the transcript,
// on the page, and "Hey Glass" asks the person's agent at once; muted
// isn't transcribed.
//
// The test plays glassd and the console: it answers the page's bridge
// calls (`meet.join`, `meet.keys`, `meet.leave`) and hands each offer to
// the relay's private API, so it can also seat someone holding the wrong
// key, and make a new media key when someone leaves. Each page's peer
// connection is found through an init script, never through the app.

import { type ChildProcess, spawn } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { dirname, extname, join } from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";
import { type Browser, type BrowserContext, chromium, expect, type Page, test } from "@playwright/test";

import { type Line, wake } from "../meet/src/transcribe.ts";

const REPO = join(dirname(fileURLToPath(import.meta.url)), "../..");
const TARGET = process.env.CARGO_TARGET_DIR ?? join(REPO, "target");
const DIST = join(REPO, "apps/meet/dist");
const ORIGIN = "http://meet.localhost:7398";
/** UI-7: Join to the others on screen. */
const JOIN_BUDGET_MS = 2000;
/** UI-7: a page in a call, video decoding. */
const CALL_MEMORY_BUDGET_MB = 250;
/** UI-7: any app's cold launch and memory before use. */
const LAUNCH_BUDGET_MS = 1000;
const MEMORY_BUDGET_MB = 150;
/** How long a replaced media key still opens frames (frames.ts). */
const OLD_KEY_MS = 2000;
const SHOTS = join(REPO, "apps/e2e/results/meet-shots");
/** UI-7: a page in a transcribed call, its speech model loaded (ADR 0045 T2). */
const TRANSCRIBED_MEMORY_BUDGET_MB = 500;
const TYPES: Record<string, string> = {
  ".html": "text/html",
  ".js": "text/javascript",
  ".css": "text/css",
  ".json": "application/json",
  ".wasm": "application/wasm",
};

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

/** Every person's browser context, closed after each test so the next measures only its own pages. */
const contexts: BrowserContext[] = [];

test.afterEach(async () => {
  for (const context of contexts.splice(0)) await context.close().catch(() => undefined);
});

const base64 = (bytes: Uint8Array) => Buffer.from(bytes).toString("base64");
/** A media key's id, as glassd names them (`md-` and 16 hex digits). */
const keyId = (key: Uint8Array) => `md-${Buffer.from(key.subarray(0, 8)).toString("hex")}`;

/** Each room's media keys, current first, as `meet.keys` answers. */
const roomKeys = new Map<string, Uint8Array[]>();
/** Rooms whose test makes a new key after someone leaves, and those where one is due and not made yet. */
const rekeyed = new Set<string>();
const keyDue = new Set<string>();
/** The current key id each person's last `meet.keys` got, by `room/participant`. */
const served = new Map<string, string>();

interface Seat {
  room: string;
  participant: number;
  mediaKey: Uint8Array;
  /** The display name the console passes the relay, if any. */
  name?: string;
  /** A transcribed meeting (ADR 0045): what the page adds to its transcript, and every call it makes, in order. */
  transcript?: { added: Line[][]; calls: string[] };
}

/** One person's Meet page, with the test standing in for glassd. */
async function person(browser: Browser, seat: Seat): Promise<Page> {
  const context = await browser.newContext();
  contexts.push(context);
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
      const { action, input } = route.request().postDataJSON() as { action: string; input: { offer?: string; lines?: Line[] } };
      seat.transcript?.calls.push(action);
      if (action === "meet.list" && seat.transcript) {
        return route.fulfill({ json: { ok: true, text: "1 meeting", data: { meetings: [{ id: seat.room, transcribed: true }] } } });
      }
      if (action === "meet.transcribe" && seat.transcript) {
        const lines = input.lines ?? [];
        seat.transcript.added.push(lines);
        // As glassd answers: the requests to the person's agent among them.
        const asked = lines.map((l) => wake(l.text)).filter((r): r is string => !!r);
        return route.fulfill({ json: { ok: true, text: `Added ${lines.length} lines`, data: { lines: lines.length, asked } } });
      }
      if (action === "meet.join") {
        const response = await fetch(`${api}/rooms/${seat.room}/participants`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ participant: seat.participant, offer: input.offer, ...(seat.name ? { name: seat.name } : {}) }),
        });
        const body = (await response.json()) as { answer?: string; error?: string };
        if (!response.ok || !body.answer) return route.fulfill({ json: { ok: false, text: body.error ?? "refused", data: null } });
        return route.fulfill({
          json: {
            ok: true,
            text: "Joined",
            data: {
              answer: body.answer,
              media_key: base64(seat.mediaKey),
              media_key_id: keyId(seat.mediaKey),
              participant: seat.participant,
              ...(seat.transcript ? { transcribed: true } : {}),
            },
          },
        });
      }
      if (action === "meet.keys") {
        const keys = roomKeys.get(seat.room) ?? [seat.mediaKey];
        served.set(`${seat.room}/${seat.participant}`, keyId(keys[0]!));
        return route.fulfill({
          // As glassd answers: whether a new key is still due. The page
          // asks until there is one or none is (ADR 0052).
          json: {
            ok: true,
            text: "Keys",
            data: { meeting: seat.room, media_keys: keys.map((k) => ({ id: keyId(k), key: base64(k) })), due: keyDue.has(seat.room) },
          },
        });
      }
      if (action === "meet.leave") {
        await fetch(`${api}/rooms/${seat.room}/participants/${seat.participant}`, { method: "DELETE" });
        // Someone who left mustn't read what follows: in a room whose
        // keys a test hands out, a new one is due from here until the
        // test has made it. (This said "none is due" always: the others,
        // who ask a quarter of a second after hearing someone left, were
        // told so before the test made the key 300 ms on, stopped asking,
        // and the test waited out its 15 s. It failed every run.)
        if (rekeyed.has(seat.room)) keyDue.add(seat.room);
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

interface Inbound {
  kind: string;
  track: string;
  framesDecoded: number;
  audioSamples: number;
  packets: number;
}

/** What the page receives, one entry per incoming stream. */
async function inbound(page: Page): Promise<Inbound[]> {
  return page.evaluate(async () => {
    const out: Inbound[] = [];
    for (const pc of (window as unknown as { __pcs: RTCPeerConnection[] }).__pcs) {
      (await pc.getStats()).forEach((s) => {
        if (s.type !== "inbound-rtp") return;
        out.push({
          kind: s.kind,
          track: s.trackIdentifier ?? "",
          framesDecoded: s.framesDecoded ?? 0,
          audioSamples: (s.totalSamplesReceived ?? 0) - (s.concealedSamples ?? 0),
          packets: s.packetsReceived ?? 0,
        });
      });
    }
    return out;
  });
}

interface Received {
  framesDecoded: number;
  audioSamples: number;
}

/** What the page has decoded from everyone so far. */
async function received(page: Page): Promise<Received> {
  const streams = await inbound(page);
  return {
    framesDecoded: streams.filter((s) => s.kind === "video").reduce((n, s) => n + s.framesDecoded, 0),
    audioSamples: streams.filter((s) => s.kind === "audio").reduce((n, s) => n + s.audioSamples, 0),
  };
}

/** How many incoming videos have decoded more than `frames` frames, and audios more than `samples` samples. */
async function decoding(page: Page, frames = 0, samples = 0): Promise<{ videos: number; audios: number }> {
  const streams = await inbound(page);
  return {
    videos: streams.filter((s) => s.kind === "video" && s.framesDecoded > frames).length,
    audios: streams.filter((s) => s.kind === "audio" && s.audioSamples > samples).length,
  };
}

/** Frames decoded of the track the large screen view shows. */
async function screenFrames(page: Page): Promise<number> {
  const track = await page.evaluate(() => {
    const video = document.querySelector<HTMLVideoElement>("video[data-test=screen]");
    return (video?.srcObject as MediaStream | null)?.getVideoTracks()[0]?.id ?? "";
  });
  if (!track) return 0;
  return (await inbound(page)).filter((s) => s.track === track).reduce((n, s) => n + s.framesDecoded, 0);
}

/** The largest renderer's resident memory, in MB: a page decoding video. */
async function largestRenderer(browser: Browser): Promise<number> {
  const cdp = await browser.newBrowserCDPSession();
  const info = (await cdp.send("SystemInfo.getProcessInfo")) as { processInfo: { type: string; id: number }[] };
  await cdp.detach();
  return Math.max(
    ...info.processInfo
      .filter((p) => p.type === "renderer")
      .map((p) => Number(/VmRSS:\s+(\d+)/.exec(readFileSync(`/proc/${p.id}/status`, "utf8"))?.[1] ?? 0) / 1024),
  );
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
    .poll(async () => (await inbound(eve)).filter((s) => s.kind === "video").reduce((n, s) => n + s.packets, 0), { timeout: 10_000 })
    .toBeGreaterThan(50);
  // ...but no frame opens with another key, so none is decoded.
  await alice.waitForTimeout(1000);
  expect((await received(eve)).framesDecoded).toBe(0);
  expect((await received(alice)).framesDecoded).toBe(0);
  await eve.screenshot({ path: join(SHOTS, "wrong-key.png") });
});

test("cf21 ui7 three people each see and hear the other two, the third joining in under two seconds", async ({ browser }) => {
  const mediaKey = key();
  const ana = await person(browser, { room: "triad", participant: 0, mediaKey, name: "Ana Lima" });
  const bo = await person(browser, { room: "triad", participant: 1, mediaKey, name: "Bo Chen" });
  const third = await person(browser, { room: "triad", participant: 2, mediaKey });
  await ana.click("[data-test=join]");
  await bo.click("[data-test=join]");
  await expect.poll(async () => (await decoding(bo)).videos, { timeout: 10_000 }).toBe(1);

  // The third person joins: from Join to both others decoded on their
  // page, the relay giving them one and a renegotiation the other.
  const started = Date.now();
  await third.click("[data-test=join]");
  await expect.poll(async () => (await decoding(third)).videos, { timeout: 10_000, intervals: [50] }).toBe(2);
  const joinMs = Date.now() - started;
  // And the two already there see the newcomer, after offering a line each.
  await expect.poll(async () => Math.min((await decoding(ana)).videos, (await decoding(bo)).videos), { timeout: 10_000, intervals: [50] }).toBe(2);
  const seenMs = Date.now() - started;
  console.log(`meet: the third person sees both others ${joinMs} ms after Join; both others see them at ${seenMs} ms`);
  expect(joinMs, "joining a three-person meeting (UI-7)").toBeLessThan(JOIN_BUDGET_MS);
  expect(seenMs, "the others seeing a newcomer (UI-7)").toBeLessThan(JOIN_BUDGET_MS);

  for (const page of [ana, bo, third]) {
    await expect.poll(async () => decoding(page, 30, 48_000), { timeout: 10_000 }).toEqual({ videos: 2, audios: 2 });
    await expect(page.locator("[data-test=tile]")).toHaveCount(2);
    await expect(page.locator("video[data-test=other]")).toHaveCount(2);
    await expect(page.locator("[data-test=count]")).toHaveText("3 people");
  }
  // Names the console passed; "Participant N" where it passed none.
  await expect(third.locator("[data-test=tile] .who")).toHaveText(["Ana Lima", "Bo Chen"]);
  await expect(ana.locator("[data-test=tile] .who")).toHaveText(["Bo Chen", "Participant 3"]);

  const callMb = await largestRenderer(browser);
  console.log(`meet: a renderer in a three-person call uses ${callMb.toFixed(0)} MB`);
  expect(callMb, "a meeting's memory (UI-7)").toBeLessThan(CALL_MEMORY_BUDGET_MB);
  await third.screenshot({ path: join(SHOTS, "three-people.png") });
});

test("cf21 a shared screen reaches the others and is shown large", async ({ browser }) => {
  const mediaKey = key();
  const ana = await person(browser, { room: "review", participant: 0, mediaKey, name: "Ana Lima" });
  const bo = await person(browser, { room: "review", participant: 1, mediaKey, name: "Bo Chen" });
  const cy = await person(browser, { room: "review", participant: 2, mediaKey, name: "Cy Okafor" });
  // All three at once: their renegotiations overlap, and the relay asks
  // a sender again for a key frame its encoder ignored as too soon.
  for (const page of [ana, bo, cy]) await page.click("[data-test=join]");
  for (const page of [ana, bo, cy]) await expect.poll(async () => (await decoding(page)).videos, { timeout: 10_000 }).toBe(2);

  // Ana shares: the engine's own screen capture, sealed like her camera.
  await ana.click("[data-test=share]");
  await expect(ana.locator("[data-test=share]")).toHaveText("Stop sharing");
  for (const page of [bo, cy]) {
    await expect(page.locator("video[data-test=screen]")).toBeVisible({ timeout: 10_000 });
    await expect(page.locator(".screen .who")).toHaveText("Ana Lima's screen");
    await expect.poll(async () => screenFrames(page), { timeout: 10_000 }).toBeGreaterThan(10);
    // Everyone's camera is still there, beside it.
    await expect(page.locator(".strip [data-test=tile]")).toHaveCount(2);
  }
  await expect(ana.locator("video[data-test=screen]")).toHaveCount(0);
  await bo.screenshot({ path: join(SHOTS, "screen-shared.png") });

  await ana.click("[data-test=share]");
  await expect(ana.locator("[data-test=share]")).toHaveText("Share screen");
  for (const page of [bo, cy]) {
    await expect(page.locator("video[data-test=screen]")).toHaveCount(0, { timeout: 10_000 });
    await expect(page.locator(".grid [data-test=tile]")).toHaveCount(2);
  }
});

test("cf21 after someone leaves the others switch to the new media key and still see each other", async ({ browser }) => {
  rekeyed.add("rekey");
  const first = key();
  const ana = await person(browser, { room: "rekey", participant: 0, mediaKey: first });
  const bo = await person(browser, { room: "rekey", participant: 1, mediaKey: first });
  const cy = await person(browser, { room: "rekey", participant: 2, mediaKey: first });
  for (const page of [ana, bo, cy]) await page.click("[data-test=join]");
  for (const page of [ana, bo, cy]) await expect.poll(async () => (await decoding(page)).videos, { timeout: 10_000 }).toBe(2);

  // Cy leaves; a moment later a remaining computer has made a new key
  // (the console's job, played here), which Cy never gets.
  await cy.click("[data-test=leave]");
  await expect(cy.locator("[data-test=message]")).toContainText("You left");
  await ana.waitForTimeout(300);
  const second = key();
  roomKeys.set("rekey", [second, first]);
  keyDue.delete("rekey");
  await expect
    .poll(() => [served.get("rekey/0"), served.get("rekey/1")], { timeout: 15_000 })
    .toEqual([keyId(second), keyId(second)]);

  // Past the old key's two seconds, frames sealed with it no longer
  // open: Ana and Bo still decoding each other means both seal with the
  // new key.
  await ana.waitForTimeout(OLD_KEY_MS + 500);
  const before = await Promise.all([ana, bo].map(received));
  await ana.waitForTimeout(1500);
  const after = await Promise.all([ana, bo].map(received));
  for (const i of [0, 1]) expect(after[i]!.framesDecoded - before[i]!.framesDecoded).toBeGreaterThan(15);
});

test("adr0045 t2 what I say is transcribed on this computer, Hey Glass asks my agent at once, and muted isn't transcribed", async () => {
  test.skip(!existsSync(join(DIST, "speech/encoder.onnx")), "no speech model in Meet (tools/fetch-speech-model.sh, then make apps-build)");
  // The engine's microphone plays the recording, over and over: "I'll send
  // the deck after this call." then "Hey Glass, find the ABC report and
  // send it to Susan." (meet/test/fixtures/README.md).
  const browser = await chromium.launch({
    ...test.info().project.use.launchOptions,
    args: [
      ...(test.info().project.use.launchOptions?.args ?? []),
      `--use-file-for-fake-audio-capture=${join(REPO, "apps/meet/test/fixtures/hey-glass.wav")}`,
    ],
  });
  try {
    const transcript = { added: [] as Line[][], calls: [] as string[] };
    const page = await person(browser, { room: "launch-review", participant: 0, mediaKey: key(), transcript });
    await expect(page.locator("[data-test=transcribed]")).toContainText("This meeting is transcribed");
    await page.click("[data-test=join]");
    await expect(page.locator("[data-test=transcribing]")).toHaveAttribute("data-hearing", "on", { timeout: 20_000 });

    // Both sentences become lines; the request goes the moment it's heard.
    const said = () => transcript.added.flat().map((l) => l.text);
    await expect.poll(() => said().some((t) => /^hey,? glass,? find the ABC report and send it to Susan\.?$/i.test(t)), { timeout: 30_000 }).toBe(true);
    await expect.poll(() => said().some((t) => /send the deck after this call/i.test(t)), { timeout: 20_000 }).toBe(true);
    const asking = transcript.added.find((call) => call.some((l) => wake(l.text)));
    expect(wake(asking!.at(-1)!.text), "a request is the last line of its call: it went at once").toMatch(/find the ABC report/i);
    await expect(page.locator("[data-test=asked]")).toContainText(/Your agent is on it: find the ABC report and send it to Susan/i);
    await expect(page.locator("[data-test=heard]")).toContainText("You:");
    await page.screenshot({ path: join(SHOTS, "transcribed-call.png") });
    const memoryMb = await largestRenderer(browser);
    console.log(`meet: transcribed call, renderer ${memoryMb.toFixed(0)} MB; ${said().length} lines so far`);
    expect(memoryMb, "a transcribed call's memory (UI-7)").toBeLessThan(TRANSCRIBED_MEMORY_BUDGET_MB);

    // Muted, nothing more is transcribed: what was said before goes when leaving, nothing after.
    await page.click("[data-test=mic]");
    const muted = Date.now();
    await expect(page.locator("[data-test=transcribing]")).toContainText("not you while muted");
    await page.waitForTimeout(9_000);
    await page.click("[data-test=leave]");
    await expect(page.locator("[data-test=message]")).toContainText("You left");
    await expect.poll(() => transcript.calls.includes("meet.leave"), { timeout: 10_000 }).toBe(true);
    const lines = transcript.added.flat();
    const after = lines.filter((l) => Date.parse(l.at) > muted);
    expect(after, "nothing heard after muting").toEqual([]);
    expect(transcript.calls.lastIndexOf("meet.transcribe"), "the last lines go before leaving").toBeLessThan(transcript.calls.indexOf("meet.leave"));
    for (const line of lines) expect(line.text.length).toBeGreaterThan(0);
  } finally {
    await browser.close();
  }
});

test("ui7 meet launches cold in under a second and stays under 150 MB before joining", async ({ browser }) => {
  const launches: number[] = [];
  let memoryMb = 0;
  for (let run = 0; run < 3; run += 1) {
    const started = Date.now();
    const page = await person(browser, { room: "lobby", participant: 0, mediaKey: key() });
    launches.push(Date.now() - started);
    if (run === 2) {
      await page.locator("[data-test=join]").hover();
      await page.waitForTimeout(300);
      memoryMb = await largestRenderer(browser);
    }
    await page.context().close();
  }
  const launchMs = [...launches].sort((a, b) => a - b)[1]!;
  console.log(`meet: cold launch ${launchMs} ms (runs ${launches.join(", ")}), renderer ${memoryMb.toFixed(0)} MB`);
  expect(launchMs, "Meet's cold launch (UI-7)").toBeLessThan(LAUNCH_BUDGET_MS);
  expect(memoryMb, "Meet's memory (UI-7)").toBeLessThan(MEMORY_BUDGET_MB);
});
