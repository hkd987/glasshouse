// Sharing an app a person made back to the org's store, end to end (ADR
// 0032, R2; GA-8, GA-13, DV-4). On the console suite's enrolled computer
// the person's own Claude Code remixes Mail to say "Write" and shares it
// with everyone (Ask: the desktop approves it). The console checks and
// rebuilds it, and IT reviews it: what changed, what it does beyond Mail,
// the checks, and that a person must review it because it sends mail. IT
// approves it for everyone, available; the computer gets the signed
// catalog and the console's bundle, the Store lists it with its maker,
// the person installs it and it opens on its own origin. The test fails
// when the share's round trip or approval to the computer takes too long.

import { mkdirSync, writeFileSync } from "node:fs";
import { userInfo } from "node:os";
import { join } from "node:path";
import { type Browser, type Page, expect, test } from "@playwright/test";

import { CONSOLE_SHOTS, CONSOLE_URL, STORE_PORT, STORE_URL } from "./console-env.ts";
import { type ApprovalView, DEV, audit, glassctl, glassctlJson, mcp, outcome } from "./env.ts";

/** From the desktop's approval to the console's answer: checked, built and kept for review. */
const SHARE_BUDGET_MS = 15_000;
/** AD-3, as console.e2e.ts has it: a change IT makes is on an online computer within this. */
const PROPAGATION_BUDGET_MS = 5000;
const ADMIN = userInfo().username;
/** The Store's name for it, and the catalog id the console gives it. */
const NAME = "Mail, with Write";
const APP = "mail-with-write";
const APP_URL = `http://${APP}.localhost:${STORE_PORT}/`;
const NOTE = "Mail, with the compose button saying Write";

interface ToolResult {
  isError?: boolean;
  content: { text: string }[];
  structuredContent?: Record<string, unknown>;
}

const text = (r: ToolResult) => r.content.map((c) => c.text).join("\n");

/** One call to the studio's tools, as Claude Code makes it. */
async function tool(socket: string, name: string, args: unknown): Promise<ToolResult> {
  const [result] = (await mcp(socket, [{ name, arguments: args }])) as ToolResult[];
  return result!;
}

/** IT, signed in to the console in a context of their own. */
async function signIn(browser: Browser): Promise<{ page: Page; close: () => Promise<void> }> {
  const context = await browser.newContext();
  const page = await context.newPage();
  page.on("pageerror", (e) => console.error(`page error: ${e.message}`));
  await page.goto(CONSOLE_URL);
  await page.click(`button[data-user="${ADMIN}"]`);
  await page.waitForSelector("html[data-ready=true][data-page=policy]");
  return { page, close: () => context.close() };
}

async function shot(page: Page, name: string): Promise<void> {
  mkdirSync(CONSOLE_SHOTS, { recursive: true });
  await page.screenshot({ path: join(CONSOLE_SHOTS, `${name}.png`) });
}

const measured: Record<string, number> = {};

test.afterAll(() => {
  writeFileSync(join(DEV, "..", "measured-studio.json"), JSON.stringify(measured, null, 2));
  console.log(JSON.stringify(measured, null, 2));
});

test("ga8 ga13 dv4 a remix of Mail is shared, reviewed in the console and installed from the Store", async ({ browser }) => {
  // 1. The person's Claude Code remixes Mail: "Write" for "New message".
  const task = glassctlJson<{ id: string; driver_socket: string }>(
    "run",
    "claude-code",
    "Make Mail say Write and share it with everyone",
    "--title",
    NAME,
    "--yes",
  );
  const socket = task.driver_socket;
  const source = await tool(socket, "apps_source", { app: "mail" });
  expect(source.isError ?? false, text(source)).toBe(false);
  const data = source.structuredContent as { manifest: Record<string, unknown>; files: Record<string, string> };
  const main = data.files["src/main.ts"]!;
  expect(main).toContain('"New message",');
  const saved = await tool(socket, "apps_save", {
    base: "mail",
    manifest: data.manifest,
    files: { ...data.files, "src/main.ts": main.replace('"New message",', '"Write",') },
    note: "Write instead of New message",
  });
  expect(saved.isError ?? false, text(saved)).toBe(false);
  const mine = String(saved.structuredContent?.app);
  expect(mine).toMatch(/^my-/);

  // 2. Sharing sends the source off the computer: it asks first (Ask).
  const sharing = tool(socket, "apps_share", { app: mine, to: "org", name: NAME, note: NOTE });
  let ask: ApprovalView | undefined;
  await expect
    .poll(() => (ask = glassctlJson<ApprovalView[]>("approvals").find((a) => a.summary === `Share ${NAME} for review`)), {
      timeout: 10_000,
    })
    .toBeTruthy();
  expect(ask?.agent).toBe("claude-code");
  const approved = Date.now();
  glassctl("approve", ask!.id);
  const shared = await sharing;
  measured.share_round_trip_ms = Date.now() - approved;
  glassctl("stop", task.id);
  expect(shared.isError ?? false, text(shared)).toBe(false);
  expect(shared.structuredContent?.id).toBe(APP);
  expect(text(shared)).toContain(`Sent ${NAME} to IT for review`);
  const call = audit().find((r) => r.event.kind === "action" && r.event.action === "apps.share" && outcome(r) === "completed");
  expect(call?.event.decision?.tier).toBe("ask");
  expect(measured.share_round_trip_ms, "share: approved on the desktop to kept for review").toBeLessThan(SHARE_BUDGET_MS);

  // 3. IT reviews it in the console.
  const { page, close } = await signIn(browser);
  await page.goto(`${CONSOLE_URL}#/catalog/${APP}`);
  await page.waitForSelector("html[data-ready=true][data-page=catalog]", { state: "attached" });
  await expect(page.locator("h1")).toHaveText(NAME);
  await expect(page.locator(".crumbs")).toContainText("Waiting for review");
  const made = page.locator("[data-test=made-by]");
  await expect(made).toContainText("from mail 0.1.0");
  await expect(made).toContainText("for everyone in the organization");
  await expect(page.locator("[data-test=note]")).toContainText(NOTE);
  // DV-4: Mail sends mail, which can't be undone.
  const person = page.locator("[data-test=needs-person]");
  await expect(person).toContainText("A person must review this: it can");
  await expect(person).toContainText("Approving it is that review.");
  // A remix that only changes words does nothing beyond Mail.
  await expect(page.locator("[data-test=beyond] h2")).toHaveText("Nothing beyond mail");
  // The console's own checks, every one passed.
  const checks = page.locator("[data-test=checks] [data-test=check]");
  expect(await checks.count()).toBeGreaterThan(0);
  await expect(page.locator("[data-test=checks] [data-test=check][data-ok=false]")).toHaveCount(0);
  // The change, line by line, from the console's own Mail.
  const file = page.locator('[data-test=changes] details[data-file="src/main.ts"]');
  await expect(file).toHaveCount(1);
  if ((await file.getAttribute("open")) === null) await file.locator("summary").click();
  const write = file.locator(".diff .ln.add", { hasText: '"Write",' });
  await expect(write).toHaveCount(1);
  await expect(write).toHaveText(/^\+/);
  await expect(file.locator(".diff .ln.del", { hasText: '"New message",' })).toHaveCount(1);
  // Shared with everyone: the decision starts there.
  await expect(page.locator("select[data-test=group]")).toHaveValue("everyone");
  await shot(page, "studio-review-waiting");
  await file.scrollIntoViewIfNeeded();
  await shot(page, "studio-review-diff");

  await page.locator("select[data-test=mode]").selectOption("available");
  await page.click("button[data-test=approve]");
  const decided = Date.now();
  await expect(page.locator(".notice")).toContainText("Approved. Catalog v");
  await expect(page.locator(".crumbs")).toContainText("Approved");
  await page.evaluate(() => {
    window.scrollTo(0, 0);
    document.body.scrollTo(0, 0);
    document.querySelector("main.content")?.scrollTo(0, 0);
  });
  await shot(page, "studio-review-approved");
  await close();

  // 4. On the computer: the signed catalog, then the console's bundle, kept
  // only because its digest matches.
  const installed = new RegExp(`^App ${APP} \\S+ from the org's catalog installed`);
  while (!audit().some((r) => installed.test(r.event.summary))) {
    expect(Date.now() - decided, "approved to on the computer").toBeLessThan(2 * PROPAGATION_BUDGET_MS);
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
  measured.approve_to_computer_ms = Date.now() - decided;
  expect(measured.approve_to_computer_ms, "approved in the console to on the computer (AD-3)").toBeLessThan(PROPAGATION_BUDGET_MS);

  const store = await browser.newPage();
  await expect(async () => {
    await store.goto(STORE_URL);
    await expect(store.locator("html[data-ready=true]")).toHaveCount(1, { timeout: 2000 });
    await expect(store.locator(`article[data-id=${APP}]`)).toHaveCount(1, { timeout: 500 });
  }).toPass({ timeout: 10_000 });
  const card = store.locator(`article[data-id=${APP}]`);
  await expect(card).toContainText(NAME);
  await expect(card.locator(".publisher")).toHaveText(/^Made by .+ · remix of mail$/);
  await expect(card.locator(".summary")).toHaveText(NOTE);
  await expect(card.locator("button")).toHaveText("Install");
  await card.scrollIntoViewIfNeeded();
  await store.screenshot({ path: join(CONSOLE_SHOTS, "studio-store-available.png") });
  await card.locator("button").click();
  await expect(store.locator(".banner.done")).toContainText(`Installed ${NAME}`);
  await expect(store.locator("section", { hasText: "Yours" }).locator(`article[data-id=${APP}]`)).toHaveCount(1);
  await store.screenshot({ path: join(CONSOLE_SHOTS, "studio-store-installed.png") });

  // Installed, it opens on its own origin, built by the console.
  const app = await browser.newPage();
  await app.goto(APP_URL);
  await expect(app.locator("html[data-ready=true]")).toHaveCount(1);
  await expect(app).toHaveTitle(NAME);
  await expect(app.locator("[data-test=compose]")).toHaveText("Write");
  await app.screenshot({ path: join(CONSOLE_SHOTS, "studio-app-opened.png") });
  await app.close();

  // As the other specs leave it: nothing of the catalog's installed.
  await store.locator(`article[data-id=${APP}] button`, { hasText: "Remove" }).click();
  await expect(store.locator(".banner.done")).toContainText(`Removed ${NAME}`);
  await store.close();
});
