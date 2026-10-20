// Artifacts (AR-1 to AR-5, ADR 0033) on the console suite's enrolled
// computer, with Priya on another computer (crates/apps/chat/examples/
// colleague.rs). The person's own agent shares a page with Priya and into
// their conversation; Artifacts lists it and opens it in under a second
// (AR-2), sealed on its own origin: a fetch and an image from inside the
// frame are refused by its CSP, and opened on its own it has no storage.
// The person pins comments to elements, the agent reads them and shares
// version 2, and the comment says it was about v1. A page that navigates
// itself is put back. Chat shows the share as a card that opens it.

import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readdirSync, readFileSync, statSync } from "node:fs";
import { userInfo } from "node:os";
import { join } from "node:path";
import { type Browser, type FrameLocator, type Page, expect, test } from "@playwright/test";

import { ARTIFACTS_URL, CHAT_URL, CONSOLE_SHOTS, CONSOLE_STATE, STORE_PORT } from "./console-env.ts";
import { type ApprovalView, BIN, DEV, audit, glassctl, glassctlJson, mcp, sleep } from "./env.ts";

test.describe.configure({ mode: "serial" });

const me = userInfo().username;
/** AR-2: from the click in the list to the page painted in its frame. */
const OPEN_BUDGET_MS = 1000;
const RUNS = 3;
const MAIL = `http://mail.localhost:${STORE_PORT}`;
/** Where the comment on Q3's total is pinned: the shim's path from the body. */
const TOTAL = "main:nth-of-type(1) > table:nth-of-type(1) > tbody:nth-of-type(1) > tr:nth-of-type(3) > td:nth-of-type(2)";
const TITLE = "main:nth-of-type(1) > h1:nth-of-type(1)";

/** The page the agent shares. Its own script tries the network, an image and storage, and shows what happened. */
function q3(version: number): string {
  const services = version === 1 ? 186 : 228;
  return `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>Q3 forecast</title>
<style>
body{font:15px/1.5 system-ui,sans-serif;margin:36px 40px;color:#14171c}
h1{font-size:26px;margin:0 0 2px}.sub{color:#5f6875;margin:0 0 22px}
table{border-collapse:collapse;width:100%;max-width:620px}
th,td{padding:10px 12px;border-bottom:1px solid #e1e5ea;text-align:left}
th{font-size:12px;color:#5f6875;font-weight:600}
td.n{text-align:right;font-variant-numeric:tabular-nums;width:90px}
tr.total td{font-weight:700}
.bar{height:10px;border-radius:5px;background:#3d6bff}
.sandbox{margin-top:28px;font-size:12.5px;color:#5f6875}
.sandbox b{color:#14171c;font-weight:600}
</style></head>
<body><main>
<h1 id="q3-title">Q3 forecast</h1>
<p class="sub">Northwind accounts, revenue in thousands · version ${version}</p>
<table>
<thead><tr><th>Line</th><th>Q3</th><th>Share</th></tr></thead>
<tbody>
<tr><td>Licenses</td><td class="n">412</td><td><div class="bar" style="width:70%"></div></td></tr>
<tr><td>Services</td><td class="n">${services}</td><td><div class="bar" style="width:${Math.round(services / 6)}%"></div></td></tr>
<tr class="total"><td>Total</td><td class="n" id="total">${412 + services}</td><td></td></tr>
</tbody>
</table>
<p class="sandbox">Sealed: network <b id="net">…</b> · image <b id="img">…</b> · storage <b id="storage">…</b> · origin <b id="origin">…</b> · refused by <b id="csp">nothing</b></p>
</main>
<script>
const seen = [];
const say = (id, text) => { document.getElementById(id).textContent = text; };
document.addEventListener("securitypolicyviolation", (e) => {
  if (!seen.includes(e.violatedDirective)) seen.push(e.violatedDirective);
  say("csp", seen.join(", "));
});
say("origin", String(self.origin));
try { localStorage.setItem("seen", "1"); say("storage", "kept"); } catch (_) { say("storage", "none"); }
fetch("${MAIL}/", { mode: "no-cors" }).then(() => say("net", "reached"), () => say("net", "blocked"));
const img = new Image();
img.onload = () => say("img", "loaded");
img.onerror = () => say("img", "blocked");
img.src = "${MAIL}/app.css";
</script>
</body></html>`;
}

/** A page that sends itself, and what it shows, elsewhere when its button is pressed. */
const WANDERING = `<!doctype html><html><head><meta charset="utf-8"><style>body{font:15px system-ui,sans-serif;margin:36px 40px}</style></head>
<body><h1 id="wander">Vendor shortlist</h1><p>Three vendors, scored on price and support.</p>
<button id="go" onclick="location.href='http://127.0.0.1:${STORE_PORT}/collect?d='+encodeURIComponent(document.body.innerText)">See the sources</button>
</body></html>`;

/** Priya, on her own computer. */
function priya(...args: string[]): string {
  return execFileSync(
    join(BIN, "examples/colleague"),
    ["--enrollment", join(CONSOLE_STATE, "enrollment.json"), "--state", join(DEV, "../colleague"), "--as", "priya", ...args],
    { encoding: "utf8" },
  ).trim();
}

/** Every file under `dir`, for looking for text that mustn't be there. */
function everything(dir: string): string {
  return readdirSync(dir)
    .map((name) => join(dir, name))
    .map((path) => (statSync(path).isDirectory() ? everything(path) : readFileSync(path, "latin1")))
    .join("\n");
}

async function openApp(browser: Browser, url = ARTIFACTS_URL): Promise<{ page: Page; close: () => Promise<void> }> {
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  const page = await context.newPage();
  await page.goto(url);
  await page.waitForSelector("html[data-ready=true]", { state: "attached" });
  return { page, close: () => context.close() };
}

const frameOf = (page: Page): FrameLocator => page.frameLocator("[data-test=frame]");

async function shot(page: Page, name: string): Promise<void> {
  mkdirSync(CONSOLE_SHOTS, { recursive: true });
  await page.screenshot({ path: join(CONSOLE_SHOTS, `${name}.png`) });
}

type ToolResult = { isError?: boolean; content: { text: string }[]; structuredContent?: Record<string, unknown> };

/** One of the person's own agent's tasks, its calls made through the task socket. */
async function agent(title: string): Promise<{ call: (name: string, args: unknown) => Promise<ToolResult>; stop: () => void }> {
  const task = glassctlJson<{ id: string; driver_socket: string }>("run", "claude-code", title, "--title", title, "--yes");
  for (let i = 0; i < 200 && !existsSync(task.driver_socket); i++) await sleep(25);
  return {
    call: async (name, args) => ((await mcp(task.driver_socket, [{ name, arguments: args }])) as ToolResult[])[0]!,
    stop: () => glassctl("stop", task.id),
  };
}

/** Approves the ask summarized `summary`, as the person would on the desktop. */
async function approve(summary: string): Promise<ApprovalView> {
  let ask: ApprovalView | undefined;
  await expect
    .poll(() => (ask = glassctlJson<ApprovalView[]>("approvals").find((a) => a.summary === summary)), { timeout: 10_000 })
    .toBeTruthy();
  glassctl("approve", ask!.id);
  return ask!;
}

function ok(result: ToolResult): ToolResult {
  expect(result.isError ?? false, result.content.map((c) => c.text).join("\n")).toBe(false);
  return result;
}

let q3Id = "";
let conversation = "";

test("ar1 ar5 the person's agent shares a page with Priya and into their conversation; it shows up live", async ({ browser }) => {
  // Opening Artifacts makes the person's keys, as opening Chat does.
  const { page, close } = await openApp(browser);
  await expect(page.locator("[data-test=welcome]")).toContainText("No artifacts yet");
  await expect(page.locator("[data-test=shared-with-me]")).toContainText("Nothing shared with you yet");
  priya("open");
  priya("say", me, "Could you send me the Q3 forecast as a page?");

  const work = await agent("Share the Q3 forecast with Priya");
  const listed = ok(await work.call("chat_list", {}));
  const found = /\[(cv-[0-9a-f]{32})\]/.exec(listed.content.map((c) => c.text).join("\n"));
  expect(found, "the conversation with Priya").toBeTruthy();
  conversation = found![1]!;
  const sharing = work.call("artifact_share", { title: "Q3 forecast", html: q3(1), with: ["priya"], conversation, note: "Numbers as of Monday" });
  const ask = await approve("Share Q3 forecast");
  expect(ask.items.join("\n")).toContain("Priya Shah");
  const shared = ok(await sharing);
  q3Id = String(shared.structuredContent?.id ?? /cv-[0-9a-f]{32}/.exec(shared.content[0]?.text ?? "")?.[0] ?? "");
  work.stop();
  expect(q3Id).toMatch(/^cv-[0-9a-f]{32}$/);

  // News reaches the open app from an app.status wait: no reload.
  const listedRow = page.locator(`[data-test=mine] [data-artifact="${q3Id}"]`);
  await expect(listedRow).toContainText("Q3 forecast", { timeout: 5000 });
  await expect(listedRow).toContainText("You · v1");
  await expect(listedRow.locator("[data-test=label]")).toHaveText("Internal");
  // Priya's computer opens the card in their conversation, sealed to her.
  const hers = priya("read", me);
  expect(hers).toContain('"kind":"artifact"');
  expect(hers).toContain(`"item":"${q3Id}"`);
  await close();
});

test("ar2 opening an artifact paints its page in its sealed frame in under a second", async ({ browser }) => {
  const runs: number[] = [];
  for (let run = 0; run < RUNS; run++) {
    const { page, close } = await openApp(browser);
    const row = page.locator(`[data-artifact="${q3Id}"]`);
    await row.waitFor();
    const started = Date.now();
    await row.click();
    await frameOf(page).locator("#q3-title").waitFor();
    runs.push(Date.now() - started);
    if (run === RUNS - 1) {
      const frame = frameOf(page);
      // The page's own script found no network, no image from elsewhere and
      // no storage, and an origin that is no one's: the CSP holds, and the
      // frame's host resolved, since the page is there.
      await expect(frame.locator("#net")).toHaveText("blocked");
      await expect(frame.locator("#img")).toHaveText("blocked");
      await expect(frame.locator("#storage")).toHaveText("none");
      await expect(frame.locator("#origin")).toHaveText("null");
      await expect(frame.locator("#csp")).toContainText("connect-src");
      await expect(frame.locator("#csp")).toContainText("img-src");
      const src = await page.locator("[data-test=frame]").getAttribute("src");
      expect(src).toBe(`http://${q3Id.slice(3)}.artifact.localhost:${STORE_PORT}/v1`);
      expect(await page.locator("[data-test=frame]").getAttribute("sandbox")).toBe("allow-scripts");
      // The agent's by-line on its version (AU-10).
      await expect(page.locator(".version-line .attribution")).toHaveText(`by claude-code for ${me}`);
    }
    await close();
  }
  const sorted = [...runs].sort((a, b) => a - b);
  const median = sorted[Math.floor(sorted.length / 2)]!;
  console.log(`artifacts: open an artifact ${median} ms, click to the page painted (runs ${runs.join(", ")})`);
  expect(median, "AR-2: an artifact opens in under a second").toBeLessThan(OPEN_BUDGET_MS);
});

test("ar2 opened on its own, an artifact's page has an origin that is no one's and no storage", async ({ browser }) => {
  const context = await browser.newContext();
  const page = await context.newPage();
  const response = await page.goto(`http://${q3Id.slice(3)}.artifact.localhost:${STORE_PORT}/`);
  expect(response?.headers()["content-security-policy"]).toMatch(/^sandbox allow-scripts;/);
  await expect(page.locator("#q3-title")).toHaveText("Q3 forecast");
  const seen = await page.evaluate(() => {
    const tries = (f: () => unknown) => {
      try {
        f();
        return "allowed";
      } catch {
        return "refused";
      }
    };
    return {
      origin: self.origin,
      local: tries(() => localStorage.length),
      session: tries(() => sessionStorage.length),
      cookie: tries(() => document.cookie),
      indexed: tries(() => indexedDB.open("x")),
    };
  });
  expect(seen).toEqual({ origin: "null", local: "refused", session: "refused", cookie: "refused", indexed: "refused" });
  await expect(page.locator("#storage")).toHaveText("none");
  await context.close();
});

test("ar3 ar4 a pinned comment carries its element's path and its pin is drawn; the agent reads it and shares v2", async ({ browser }) => {
  const { page, close } = await openApp(browser, `${ARTIFACTS_URL}#${q3Id}`);
  const frame = frameOf(page);
  await frame.locator("#q3-title").waitFor();
  await expect(page.locator("[data-test=stage]")).toHaveAttribute("data-ready", "true");

  // Pin mode, a click on Q3's total, a comment there.
  const pin = async (target: string, text: string) => {
    await page.locator("[data-test=pin-toggle]").click();
    await expect(page.locator("[data-test=pin-toggle]")).toHaveAttribute("aria-pressed", "true");
    await frame.locator(target).click();
    await expect(page.locator(".anchor-chip")).toContainText("Pinned to what you clicked");
    await expect(page.locator("[data-test=pin-toggle]")).toHaveAttribute("aria-pressed", "false");
    await expect(page.locator("[data-test=pending-pin]")).toBeVisible();
    await page.locator("[name=comment]").fill(text);
    await page.getByRole("button", { name: "Comment", exact: true }).click();
    await expect(page.locator("[data-test=comments]")).toContainText(text);
  };
  await pin("#total", "Services looks low for Q3; check the Fabrikam renewal");
  await pin("#q3-title", "Say which months this covers");
  const first = page.locator(".pin[data-pin='1']");
  await expect(first).toBeVisible();
  await expect(page.locator(".pin[data-pin='2']")).toBeVisible();
  // Each pin sits at its element's corner, drawn by the app over the frame.
  const cell = await frame.locator("#total").boundingBox();
  const drawn = await first.boundingBox();
  expect(Math.abs(drawn!.x - (cell!.x - 11))).toBeLessThanOrEqual(3);
  expect(Math.abs(drawn!.y - (cell!.y - 11))).toBeLessThanOrEqual(3);
  // A reply under the first.
  await page.locator("[data-thread]").first().getByRole("button", { name: "Reply" }).click();
  await page.locator("[name=reply]").fill("Priya says the renewal lands in October");
  await page.locator(".reply-box").getByRole("button", { name: "Reply" }).click();
  await expect(page.locator(".comment.reply")).toContainText("lands in October");
  // What the person did in Artifacts was theirs: nothing asked.
  expect(glassctlJson<ApprovalView[]>("approvals")).toHaveLength(0);
  const commented = audit().filter((r) => r.event.kind === "action" && r.event.action === "artifact.comment" && r.event.agent === "artifacts");
  expect(commented.map((r) => r.event.decision?.tier)).toEqual(["auto", "auto", "auto"]);
  await shot(page, "artifacts-viewer-pins");

  // The agent reads the feedback, with each comment's element (Auto), and
  // answers it with version 2 (Ask).
  const work = await agent("Fix the Q3 forecast from the feedback");
  const read = ok(await work.call("artifact_read", { artifact: q3Id })).content.map((c) => c.text).join("\n");
  expect(read).toContain(`at ${TOTAL}: Services looks low for Q3`);
  expect(read).toContain(`at ${TITLE}: Say which months this covers`);
  const updating = work.call("artifact_update", { artifact: q3Id, html: q3(2), note: "Services now include the Fabrikam renewal" });
  await approve("Share a new version of Q3 forecast");
  ok(await updating);
  work.stop();

  // The open viewer moves to version 2; the comments say they were on v1,
  // and v1's pins aren't drawn on v2.
  await expect(frame.locator(".sub")).toContainText("version 2", { timeout: 5000 });
  await expect(page.locator("[data-test=versions]")).toHaveValue("2");
  await expect(page.locator("[data-test=version-badge]").first()).toHaveText("on v1");
  await expect(page.locator(".pin")).toHaveCount(0);
  await expect(page.locator(".version-line")).toContainText("Services now include the Fabrikam renewal");
  // Back on v1, its pins are.
  await page.locator("[data-test=versions]").selectOption("1");
  await expect(frame.locator(".sub")).toContainText("version 1");
  await expect(page.locator(".pin")).toHaveCount(2);
  await expect(page.locator("[data-test=version-badge]")).toHaveCount(0);
  await expect(page.locator("[data-test=pin-toggle]")).toBeDisabled();
  await close();
});

test("ar1 ar2 the person shares a page from the app; when it navigates itself it is put back", async ({ browser }) => {
  const { page, close } = await openApp(browser);
  await page.locator("[data-test=share]").click();
  const sheet = page.locator("[data-test=sheet]");
  await sheet.locator("[name=title]").fill("Vendor shortlist");
  await sheet.locator("[name=html]").fill(WANDERING);
  await sheet.locator("[name=people]").fill("Pri");
  await sheet.locator("[data-login=priya]").click();
  await expect(sheet.locator("[data-person=priya]")).toContainText("Priya Shah");
  await expect(sheet.locator("[name=label]")).toHaveValue("internal");
  await sheet.locator("[name=note]").fill("Which one should we pick?");
  await shot(page, "artifacts-share");
  await sheet.locator("[data-test=sheet-submit]").click();
  await expect(sheet).toHaveCount(0);
  await expect(page.locator("[data-test=notice]")).toHaveText("Shared Vendor shortlist with Priya Shah.");
  await expect(page.locator("[data-test=title]")).toHaveText("Vendor shortlist");
  const frame = frameOf(page);
  await frame.locator("#wander").waitFor();
  expect(glassctlJson<ApprovalView[]>("approvals")).toHaveLength(0);

  // The page sends itself elsewhere with what it shows; the viewer puts it back.
  await frame.locator("#go").click();
  await expect(page.locator("[data-test=frame-notice]")).toHaveText("This page tried to open another page; it stays here.");
  await expect(frame.locator("#wander")).toHaveText("Vendor shortlist");
  const frames = page.frames().map((f) => f.url());
  expect(frames.some((u) => u.includes("/collect"))).toBe(false);
  expect(frames.some((u) => u.includes(".artifact.localhost"))).toBe(true);
  await close();

  // The list: both, newest first; nothing shared with the person by others here.
  const list = await openApp(browser);
  await expect(list.page.locator("[data-test=mine] .row")).toHaveCount(2);
  await expect(list.page.locator("[data-test=mine] .row").first()).toContainText("Vendor shortlist");
  // Its open comments, as artifact.list counts them (replies included).
  await expect(list.page.locator(`[data-artifact="${q3Id}"] [data-test=open-comments]`)).toHaveText(/^[1-9]\d*$/);
  await expect(list.page.locator("[data-test=welcome]")).toContainText("Pick an artifact");
  await shot(list.page, "artifacts-list");
  await list.close();
});

test("ar5 Chat shows the share as a card that opens the artifact, and the console holds none of it", async ({ browser }) => {
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  const page = await context.newPage();
  await page.goto(CHAT_URL);
  await page.waitForSelector("html[data-ready=true]", { state: "attached" });
  await page.locator(`.sidebar [data-conversation="${conversation}"]`).click();
  const card = page.locator(`[data-test=artifact-card][data-item="${q3Id}"]`);
  await expect(card).toContainText("Q3 forecast");
  await expect(page.locator(".message", { has: card }).locator(".attribution")).toHaveText(`by claude-code for ${me}`);
  // A card, never the message's raw text or a file.
  await expect(page.locator("[data-test=messages]")).not.toContainText("Shared Q3 forecast");
  await shot(page, "chat-artifact-card");
  const [opened] = await Promise.all([context.waitForEvent("page"), card.getByRole("link", { name: "Open" }).click()]);
  expect(opened.url()).toBe(`${ARTIFACTS_URL}#${q3Id}`);
  await opened.waitForSelector("html[data-ready=true]", { state: "attached" });
  await expect(opened.locator("[data-test=title]")).toHaveText("Q3 forecast");
  await frameOf(opened).locator("#q3-title").waitFor();
  await context.close();

  // GT-3: the console keeps sealed blobs and messages; no title, page or comment.
  const held = everything(CONSOLE_STATE);
  for (const text of ["Q3 forecast", "Northwind accounts", "Fabrikam renewal", "Vendor shortlist", "which months this covers"]) {
    expect(held, text).not.toContain(text);
  }
});
