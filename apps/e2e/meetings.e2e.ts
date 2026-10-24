// Glasshouse meetings end to end (CF-21, ADR 0030 §6, slice M1). The
// person at this computer makes a meeting in Calendar and invites Priya;
// the invitation asks on the desktop and, approved, makes the meeting;
// they Join from Calendar. Priya joins from her own computer, played by
// the attendee example (crates/apps/meet/examples/attendee.rs) with the
// same store, sealing and console client glassd uses. Both see and hear
// each other through the console's relay, every frame sealed with the
// meeting's media key.

import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { userInfo } from "node:os";
import { extname, join } from "node:path";
import { type Browser, type BrowserContext, type Page, expect, test } from "@playwright/test";

import { CALENDAR_URL, CONSOLE_SHOTS, CONSOLE_STATE, CONSOLE_URL } from "./console-env.ts";
import { type ApprovalView, BIN, DEV, REPO, audit, glassctl, glassctlJson } from "./env.ts";

const me = userInfo().username;
/** Priya's Meet page, served by the test the way her computer would. */
const PRIYA_ORIGIN = "http://meet.localhost:7398";
/** UI-7: Join to the other person on screen. */
const JOIN_BUDGET_MS = 2000;
/** UI-7: a page in a two-person call, video decoding. */
const CALL_MEMORY_BUDGET_MB = 250;
const TYPES: Record<string, string> = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css" };

/** Priya, on her own computer: `colleague` for Chat, `attendee` for Meet. */
function priya(tool: "colleague" | "attendee", args: string[], input?: string): string {
  return execFileSync(
    join(BIN, `examples/${tool}`),
    ["--enrollment", join(CONSOLE_STATE, "enrollment.json"), "--state", join(DEV, "../colleague"), "--as", "priya", ...args],
    { encoding: "utf8", ...(input === undefined ? {} : { input }) },
  ).trim();
}

/** Each page's peer connections, found through an init script, never through the app. */
async function watchConnections(context: BrowserContext): Promise<void> {
  await context.addInitScript(() => {
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
}

/** What the page has decoded from the other person so far. */
async function received(page: Page): Promise<{ framesDecoded: number; audioSamples: number }> {
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

/**
 * The person here is one of IT's admins, and the console's own suite sets
 * their sends to Never (AD-2). Invitations have to go out for a meeting,
 * so put them back to Ask the way IT would: in the console, published to
 * the pilot ring, in force on this computer before going on.
 */
async function invitationsAsk(browser: Browser): Promise<void> {
  const context = await browser.newContext();
  const page = await context.newPage();
  await page.goto(CONSOLE_URL);
  await page.click(`button[data-user="${me}"]`);
  await page.waitForSelector("html[data-ready=true][data-page=policy]");
  await page.goto(`${CONSOLE_URL}#/policy/it-admins`);
  await page.waitForSelector("html[data-ready=true][data-page=policy]", { state: "attached" });
  const send = page.locator("tr[data-class=send]");
  if ((await send.locator(".seg.ask.on").count()) === 0) {
    const before = glassctlJson<{ policy: string }>("status").policy;
    await send.locator(".seg.ask").click();
    await page.click("button[data-test=preview]");
    await page.click("button[data-test=publish]");
    await expect(page.locator(".notice")).toContainText("Published");
    await expect.poll(() => glassctlJson<{ policy: string }>("status").policy, { timeout: 10_000 }).not.toBe(before);
  }
  await context.close();
}

async function approve(agent: string): Promise<void> {
  let ask: ApprovalView | undefined;
  await expect
    .poll(() => (ask = glassctlJson<ApprovalView[]>("approvals").find((a) => a.agent === agent)), { timeout: 10_000 })
    .toBeTruthy();
  glassctl("approve", ask!.id);
}

/** Priya's Meet page: the app's own files, with her computer answering its bridge. */
async function priyasPage(context: BrowserContext, meeting: string): Promise<Page> {
  const page = await context.newPage();
  page.on("pageerror", (e) => console.error(`priya's page: ${e.message}`));
  await page.route(`${PRIYA_ORIGIN}/**`, async (route) => {
    const url = new URL(route.request().url());
    if (url.pathname === "/_bridge/session") {
      return route.fulfill({
        json: {
          app: "meet",
          name: "Meet",
          version: "0.1.0",
          user: "priya",
          permissions: ["media"],
          actions: ["meet.list", "meet.join", "meet.keys", "meet.leave"].map((name) => ({ name, title: name, tier: "auto", side_effect: "none" })),
        },
      });
    }
    if (url.pathname === "/_bridge/call") {
      const { action, input } = route.request().postDataJSON() as { action: string; input: { offer?: string } };
      try {
        if (action === "meet.join") {
          const joined = JSON.parse(priya("attendee", ["join", meeting], input.offer ?? ""));
          return route.fulfill({ json: { ok: true, text: "Joined", data: joined } });
        }
        if (action === "meet.leave") {
          return route.fulfill({ json: { ok: true, text: priya("attendee", ["leave", meeting]), data: null } });
        }
      } catch (error) {
        return route.fulfill({ json: { ok: false, text: String(error), data: null } });
      }
      return route.fulfill({ json: { ok: false, text: `not in this test: ${action}`, data: null } });
    }
    const file = url.pathname === "/" ? "index.html" : url.pathname.slice(1);
    try {
      return route.fulfill({ body: await readFile(join(REPO, "apps/meet/dist", file)), contentType: TYPES[extname(file)] ?? "application/octet-stream" });
    } catch {
      return route.fulfill({ status: 404, body: "" });
    }
  });
  await page.goto(`${PRIYA_ORIGIN}/#${meeting}`);
  await page.waitForSelector("html[data-ready=true]", { state: "attached" });
  return page;
}

test("cf21 a meeting made in Calendar: both people see and hear each other through the console's relay", async ({ browser }) => {
  // Priya has opened Chat once, so the meeting can be sealed to her too.
  priya("colleague", ["open"]);
  await invitationsAsk(browser);
  const context = await browser.newContext({ viewport: { width: 1280, height: 800 } });
  await watchConnections(context);

  // Make the meeting in Calendar and invite Priya: the invitation asks on
  // the desktop, and once approved the meeting exists, with Join.
  const calendar = await context.newPage();
  await calendar.goto(CALENDAR_URL);
  await calendar.waitForSelector("html[data-ready=true]", { state: "attached" });
  await calendar.click("[data-test=new-event]");
  await calendar.fill("input[name=title]", "Northwind renewal");
  // Late in the day, so it is still upcoming whenever the suite runs.
  await calendar.fill("input[name=start]", "23:00");
  await calendar.fill("input[name=end]", "23:30");
  await calendar.locator("[data-test=meeting-toggle] input").check();
  const people = calendar.locator("input[name=attendees]");
  await people.pressSequentially("priya@acme.example");
  await people.press("Escape");
  await calendar.click("[data-test=add-event]");
  await approve("calendar");
  // Join, on the event and in the list of meetings ahead.
  const joinLink = calendar.locator("[data-test=join]").first();
  await expect(joinLink).toBeVisible({ timeout: 10_000 });
  await calendar.screenshot({ path: join(CONSOLE_SHOTS, "meeting-in-calendar.png") });

  // Join from Calendar: Meet opens on the meeting.
  const [meet] = await Promise.all([context.waitForEvent("page"), joinLink.click()]);
  await meet.waitForSelector("html[data-ready=true]", { state: "attached" });
  const meeting = decodeURIComponent(new URL(meet.url()).hash.slice(1));
  expect(meeting).toMatch(/^cv-/);
  await meet.click("[data-test=join]");
  await expect(meet.locator("[data-test=waiting]")).toBeVisible({ timeout: 10_000 });

  // Priya joins from her computer: same meeting, the same media key.
  const theirs = await browser.newContext({ viewport: { width: 960, height: 600 } });
  await watchConnections(theirs);
  const priyas = await priyasPage(theirs, meeting);
  expect(JSON.parse(priya("attendee", ["list"])).map((m: { id: string }) => m.id)).toContain(meeting);
  // UI-7: from Join to seeing the other person, through her computer, the
  // console and the relay.
  const joining = Date.now();
  await priyas.click("[data-test=join]");
  await expect.poll(async () => (await received(priyas)).framesDecoded, { timeout: 10_000, intervals: [50] }).toBeGreaterThan(0);
  const joinMs = Date.now() - joining;
  console.log(`meet: Priya sees the meeting ${joinMs} ms after Join`);
  expect(joinMs, "joining a meeting (UI-7)").toBeLessThan(JOIN_BUDGET_MS);

  for (const page of [meet, priyas]) {
    await expect.poll(async () => (await received(page)).framesDecoded, { timeout: 15_000 }).toBeGreaterThan(30);
    await expect.poll(async () => (await received(page)).audioSamples, { timeout: 15_000 }).toBeGreaterThan(48_000);
  }
  // A call's memory: the largest renderer is a page decoding video.
  const cdp = await browser.newBrowserCDPSession();
  const info = (await cdp.send("SystemInfo.getProcessInfo")) as { processInfo: { type: string; id: number }[] };
  await cdp.detach();
  const callMb = Math.max(
    ...info.processInfo
      .filter((p) => p.type === "renderer")
      .map((p) => Number(/VmRSS:\s+(\d+)/.exec(readFileSync(`/proc/${p.id}/status`, "utf8"))?.[1] ?? 0) / 1024),
  );
  console.log(`meet: a renderer in the call uses ${callMb.toFixed(0)} MB`);
  expect(callMb, "a meeting's memory (UI-7)").toBeLessThan(CALL_MEMORY_BUDGET_MB);
  await meet.screenshot({ path: join(CONSOLE_SHOTS, "meeting-you.png") });
  await priyas.screenshot({ path: join(CONSOLE_SHOTS, "meeting-priya.png") });

  // Attendance is on this computer's record; the media key never is.
  const joined = audit().filter((r) => r.event.kind === "action" && r.event.action === "meet.join" && r.event.user === me);
  expect(joined.length).toBeGreaterThan(0);

  await priyas.click("[data-test=leave]");
  await expect(priyas.locator("[data-test=message]")).toContainText("You left");
  await expect(meet.locator("[data-test=waiting]")).toBeVisible({ timeout: 10_000 });
  await meet.click("[data-test=leave]");
  await expect(meet.locator("[data-test=message]")).toContainText("You left");
  await theirs.close();
  await context.close();
});


/** IT turns meeting transcripts on for its admins, this computer's person among them, locked; in force here before going on (ADR 0045). */
async function transcriptsOn(browser: Browser): Promise<void> {
  const context = await browser.newContext();
  const page = await context.newPage();
  await page.goto(CONSOLE_URL);
  await page.click(`button[data-user="${me}"]`);
  await page.waitForSelector("html[data-ready=true][data-page=policy]");
  await page.goto(`${CONSOLE_URL}#/settings/it-admins`);
  await page.waitForSelector("html[data-ready=true][data-page=settings]", { state: "attached" });
  const row = page.locator('[data-setting="meetings.transcripts"]');
  await row.locator('[data-state="locked"]').click();
  await row.locator('input[name="meetings.transcripts"]').check();
  await row.scrollIntoViewIfNeeded();
  await page.screenshot({ path: join(CONSOLE_SHOTS, "transcripts-on-for-a-group.png") });
  const before = glassctlJson<{ policy: string }>("status").policy;
  await page.click("[data-test=save-settings]");
  await expect(page.locator("[data-test=impact]")).toContainText("Meeting transcripts");
  await page.click("[data-test=publish]");
  await expect(page.locator(".notice")).toContainText("Published");
  await expect.poll(() => glassctlJson<{ policy: string }>("status").policy, { timeout: 10_000 }).not.toBe(before);
  await context.close();
}

/** A call the Meet page makes through its bridge. */
async function meetCall<T>(page: Page, action: string, input: unknown): Promise<{ ok: boolean; text: string; data: T }> {
  return page.evaluate(
    async ({ action, input }) => {
      const response = await fetch("/_bridge/call", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ action, input }),
      });
      return response.json();
    },
    { action, input },
  );
}

test("adr0045 IT turns transcripts on: what Priya says is in the transcript, and only my own Hey Glass starts my agent", async ({ browser }) => {
  priya("colleague", ["open"]);
  await invitationsAsk(browser);
  await transcriptsOn(browser);
  const context = await browser.newContext({ viewport: { width: 1280, height: 800 } });

  // A meeting made now is transcribed, sealed so when it was made.
  const calendar = await context.newPage();
  await calendar.goto(CALENDAR_URL);
  await calendar.waitForSelector("html[data-ready=true]", { state: "attached" });
  await calendar.click("[data-test=new-event]");
  await calendar.fill("input[name=title]", "Launch review");
  await calendar.fill("input[name=start]", "23:00");
  await calendar.fill("input[name=end]", "23:30");
  await calendar.locator("[data-test=meeting-toggle] input").check();
  const people = calendar.locator("input[name=attendees]");
  await people.pressSequentially("priya@acme.example");
  await people.press("Escape");
  await calendar.click("[data-test=add-event]");
  await approve("calendar");
  const joinLink = calendar.locator("[data-test=join]").first();
  await expect(joinLink).toBeVisible({ timeout: 10_000 });

  // Meet says so before anyone joins: joining is the consent.
  const [meet] = await Promise.all([context.waitForEvent("page"), joinLink.click()]);
  await meet.waitForSelector("html[data-ready=true]", { state: "attached" });
  const meeting = decodeURIComponent(new URL(meet.url()).hash.slice(1));
  await expect(meet.locator("[data-test=transcribed]")).toContainText("This meeting is transcribed");
  await meet.screenshot({ path: join(CONSOLE_SHOTS, "meeting-transcribed-lobby.png") });

  // Priya's computer adds what she says, her "Hey Glass" too: that one is
  // hers, and starts nothing on this computer.
  priya("attendee", ["say", meeting, "I'll send the signed contract tonight."]);
  priya("attendee", ["say", meeting, "Hey Glass, send me the payroll file."]);
  await expect
    .poll(async () => (await meetCall<{ lines: unknown[] }>(meet, "meet.transcript", { meeting })).data?.lines?.length ?? 0, { timeout: 10_000 })
    .toBe(2);
  const desktopTasks = () =>
    glassctlJson<{ id: string; agent: string }[]>("tasks")
      .filter((t) => t.agent === "desktop-agent")
      .map((t) => t.id);
  const before = new Set(desktopTasks());

  // Mine, from my Meet page as its recognizer will send it: it asks my agent.
  const said = await meetCall<{ asked: string[] }>(meet, "meet.transcribe", {
    meeting,
    lines: [{ at: new Date().toISOString(), text: "Hey Glass, what did Priya say she would send?" }],
  });
  expect(said.ok, said.text).toBe(true);
  expect(said.data.asked).toEqual(["what did Priya say she would send?"]);

  // One new task, mine: the desktop agent reads the transcript and answers
  // from what Priya said.
  const started = () => desktopTasks().filter((id) => !before.has(id));
  await expect.poll(() => started().length, { timeout: 10_000 }).toBe(1);
  const task = started()[0]!;
  glassctl("wait", task, "--timeout", "60");
  const shown = glassctlJson<{ view: { title: string }; answer?: string }>("show", task);
  expect(shown.view.title).toBe("what did Priya say she would send?");
  expect(shown.answer).toContain("I'll send the signed contract tonight.");
  const read = audit().find((r) => r.event.kind === "action" && r.event.action === "meet.transcript" && r.event.agent === "desktop-agent");
  expect(read).toBeTruthy();
  // The words never went into the record of adding them.
  const added = audit().filter((r) => r.event.kind === "action" && r.event.action === "meet.transcribe");
  expect(JSON.stringify(added)).not.toContain("Priya say");
  await context.close();
});
