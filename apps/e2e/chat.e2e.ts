// Chat (CF-91, ADR 0030) on the console suite's enrolled computer, with
// Priya on another computer, scripted with the same store, sealing and
// console client glassd uses (crates/apps/chat/examples/colleague.rs).

import { execFileSync } from "node:child_process";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { userInfo } from "node:os";
import { join } from "node:path";
import { type Browser, type Page, expect, test } from "@playwright/test";

import { CHAT_URL, CONSOLE_SHOTS, CONSOLE_STATE } from "./console-env.ts";
import { type ApprovalView, BIN, DEV, audit, glassctl, glassctlJson, mcp } from "./env.ts";

const me = userInfo().username;
const HELLO = "Hi Priya, the Northwind terms are in";
const ANSWER = "Thanks! Reading them now.";
const AGENT = "I'll review the terms by Friday.";

/** Priya, in Chat on her own computer. */
function priya(...args: string[]): string {
  return execFileSync(
    join(BIN, "examples/colleague"),
    ["--enrollment", join(CONSOLE_STATE, "enrollment.json"), "--state", join(DEV, "../colleague"), "--as", "priya", ...args],
    { encoding: "utf8" },
  ).trim();
}

async function openChat(browser: Browser): Promise<{ page: Page; ms: number; close: () => Promise<void> }> {
  const context = await browser.newContext({ viewport: { width: 1280, height: 800 } });
  const page = await context.newPage();
  const started = Date.now();
  await page.goto(CHAT_URL);
  await page.waitForSelector("html[data-ready=true]", { state: "attached" });
  return { page, ms: Date.now() - started, close: () => context.close() };
}

/** Every file under `dir`, for looking for text that mustn't be there. */
function everything(dir: string): string {
  return readdirSync(dir)
    .map((name) => join(dir, name))
    .map((path) => (statSync(path).isDirectory() ? everything(path) : readFileSync(path, "latin1")))
    .join("\n");
}

test("cf91 two people message each other in Chat, live and sealed end to end", async ({ browser }) => {
  priya("open");
  const { page, ms, close } = await openChat(browser);
  expect(ms, "UI-7: Chat drawn with its data in under 1 s").toBeLessThan(1000);
  await page.getByRole("button", { name: "New message" }).first().click();
  await page.locator("[name=to]").fill("Pri");
  await page.locator("[data-login=priya]").click();
  await page.locator("[name=message]").fill(HELLO);
  await page.keyboard.press("Enter");
  await expect(page.locator(".pane-head h1")).toHaveText("Priya Shah");
  await expect(page.locator("[data-test=messages]")).toContainText(HELLO);
  // What the person typed in Chat posted without asking.
  expect(glassctlJson<ApprovalView[]>("approvals")).toHaveLength(0);

  // Priya's computer checks and opens it.
  expect(priya("read", me)).toContain(HELLO);

  // Her answer shows without a reload, from an app.status wait: one read
  // per change, not one per poll.
  const reads = audit().filter((r) => r.event.action === "chat.read").length;
  priya("say", me, ANSWER);
  const said = Date.now();
  await expect(page.locator("[data-test=messages]")).toContainText(ANSWER, { timeout: 5000 });
  const shown = Date.now() - said;
  console.log(`chat: a colleague's message on screen ${shown} ms after it was posted`);
  expect(shown, "CF-91: posted on one computer, on screen on another (loopback)").toBeLessThan(1000);
  // Read where it is open: no unread count on it.
  await expect(page.locator(".row.active .badge")).toHaveCount(0);
  await page.waitForTimeout(500);
  expect(audit().filter((r) => r.event.action === "chat.read").length - reads).toBeLessThanOrEqual(2);
  await page.screenshot({ path: join(CONSOLE_SHOTS, "chat-two-people.png") });

  // GT-3: the console holds neither message.
  const held = everything(CONSOLE_STATE);
  expect(held).not.toContain("Northwind terms");
  expect(held).not.toContain("Reading them now");
  await close();
});

test("cf91 au10 an agent's post waits on the desktop and shows its by-line", async ({ browser }) => {
  const { page, close } = await openChat(browser);
  await expect(page.locator(".pane-head h1")).toHaveText("Priya Shah");
  const task = glassctlJson<{ id: string; driver_socket: string }>(
    "run",
    "claude-code",
    "Tell Priya I'll review the terms by Friday",
    "--title",
    "Reply to Priya",
    "--yes",
  );
  const posting = mcp(task.driver_socket, [{ name: "chat_post", arguments: { posts: [{ to: ["priya"], text: AGENT }] } }]);
  let ask: ApprovalView | undefined;
  await expect
    .poll(() => (ask = glassctlJson<ApprovalView[]>("approvals").find((a) => a.summary === "Message Priya Shah")), { timeout: 10_000 })
    .toBeTruthy();
  expect(ask?.items[0]).toBe(`Priya Shah: ${AGENT}`);
  // Nothing went out while it waits.
  expect(priya("read", me)).not.toContain(AGENT);
  glassctl("approve", ask!.id);
  const [result] = (await posting) as { isError?: boolean; content: { text: string }[] }[];
  glassctl("stop", task.id);
  expect(result?.isError ?? false, result?.content.map((c) => c.text).join("\n")).toBe(false);

  const message = page.locator(".message", { hasText: AGENT });
  await expect(message.locator(".attribution")).toHaveText(`by claude-code for ${me}`, { timeout: 5000 });
  expect(priya("read", me)).toContain(`"attribution":"by claude-code for ${me}"`);
  const post = audit().find((r) => r.event.kind === "action" && r.event.action === "chat.post" && r.event.agent === "claude-code");
  expect(post?.event.decision?.tier).toBe("ask");
  expect(JSON.stringify(post)).not.toContain(AGENT);
  await page.screenshot({ path: join(CONSOLE_SHOTS, "chat-agent-by-line.png") });
  await close();
});

test("cf91 a channel made in Chat with its people, who read it on their computer", async ({ browser }) => {
  const { page, close } = await openChat(browser);
  await page.getByRole("button", { name: "New channel" }).click();
  await page.locator("[name=channel]").fill("northwind-deal");
  await page.locator("[name=people]").fill("Pri");
  await page.locator("[data-login=priya]").click();
  await page.getByRole("button", { name: "Make channel" }).click();
  await expect(page.locator(".pane-head h1")).toHaveText("#northwind-deal");
  await page.locator("[name=message]").fill("Kickoff at 3 in the big room");
  await page.keyboard.press("Enter");
  await expect(page.locator("[data-test=messages]")).toContainText("Kickoff at 3");
  await expect(page.locator(".sidebar .row.active")).toContainText("northwind-deal");
  expect(priya("read", "#northwind-deal")).toContain("Kickoff at 3 in the big room");
  // The console keeps the channel's name sealed (ADR 0030).
  expect(everything(CONSOLE_STATE)).not.toContain("northwind-deal");
  await page.screenshot({ path: join(CONSOLE_SHOTS, "chat-channel.png") });
  await close();
});

test("cf91 Chat keeps nothing in the page's storage", async ({ browser }) => {
  const { page, close } = await openChat(browser);
  // Both conversations shown, then what the page kept is looked at.
  await expect(page.locator("[data-test=messages]")).toContainText("Kickoff at 3");
  await page.locator(".sidebar .row", { hasText: "Priya Shah" }).click();
  await expect(page.locator("[data-test=messages]")).toContainText(HELLO);
  const stored = await page.evaluate(async () => {
    const parts: unknown[] = [{ ...localStorage }, { ...sessionStorage }, await indexedDB.databases()];
    for (const name of await caches.keys()) {
      const cache = await caches.open(name);
      for (const request of await cache.keys()) parts.push(await (await cache.match(request))?.text());
    }
    return JSON.stringify(parts);
  });
  expect(stored).not.toContain("Northwind");
  expect(stored).not.toContain("Friday");
  expect(stored).not.toContain("Kickoff");
  await close();
});
