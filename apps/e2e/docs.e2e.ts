// Docs (DC-1, DC-3, DC-4, DC-6, ADR 0035) on the console suite's enrolled
// computer. The person makes a doc shared with Priya, choosing what she may
// do, and writes in it with the editor's Markdown-ish rules. A second page
// sees what the first types within the budget, through glassd's live
// session (`/_bridge/live`, not an action: no audit record per keystroke).
// The person's agent reads the doc as Markdown. A comment anchors at a word
// and stays on it while someone types before it. A doc opens in under a
// second (DC-1).
//
// The second page is a second window on the same bridge: each page names
// itself to glassd's live session (`client`), so each hears the other's
// typing and never its own.

import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync } from "node:fs";
import { userInfo } from "node:os";
import { join } from "node:path";
import { type Browser, type Page, expect, test } from "@playwright/test";

import { CONSOLE_SHOTS, CONSOLE_STATE, DOCS_URL } from "./console-env.ts";
import { type ApprovalView, BIN, DEV, audit, glassctl, glassctlJson, mcp, sleep } from "./env.ts";

test.describe.configure({ mode: "serial" });

const me = userInfo().username;
/** DC-1: from the click in the list to the doc drawn in the editor. */
const OPEN_BUDGET_MS = 1000;
/** DC-3: from the last key typed on one page to it on the other. */
const LIVE_BUDGET_MS = 1000;
const RUNS = 3;

let docId = "";

/** Priya, on her own computer. */
function priya(...args: string[]): string {
  return execFileSync(
    join(BIN, "examples/colleague"),
    ["--enrollment", join(CONSOLE_STATE, "enrollment.json"), "--state", join(DEV, "../colleague"), "--as", "priya", ...args],
    { encoding: "utf8" },
  ).trim();
}

async function openApp(browser: Browser, url: string): Promise<{ page: Page; close: () => Promise<void> }> {
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  const page = await context.newPage();
  await page.goto(url);
  await page.waitForSelector("html[data-ready=true]", { state: "attached" });
  return { page, close: () => context.close() };
}

/** The doc open and drawn on `page`. */
async function opened(page: Page): Promise<void> {
  await expect(page.locator(`[data-test=doc][data-ready=true][data-open="${docId}"]`)).toBeVisible();
}

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

function ok(result: ToolResult): ToolResult {
  expect(result.isError ?? false, result.content.map((c) => c.text).join("\n")).toBe(false);
  return result;
}

/** The Docs app's own calls in the audit log. */
const docsCalls = () => audit().filter((r) => r.event.kind === "action" && r.event.agent === "docs");

/** Selects `word` in the editor, as a person dragging over it would. */
async function select(page: Page, word: string): Promise<void> {
  await page.evaluate((w) => {
    const root = document.querySelector<HTMLElement>("[data-test=editor]");
    if (!root) throw new Error("no editor");
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
    for (let n = walker.nextNode(); n; n = walker.nextNode()) {
      const i = n.textContent?.indexOf(w) ?? -1;
      if (i < 0) continue;
      root.focus();
      const range = document.createRange();
      range.setStart(n, i);
      range.setEnd(n, i + w.length);
      const selection = getSelection();
      selection?.removeAllRanges();
      selection?.addRange(range);
      return;
    }
    throw new Error(`no ${w} in the doc`);
  }, word);
}

test("dc4 the person makes a doc shared with Priya, choosing what she may do, and writes in it", async ({ browser }) => {
  priya("open");
  const { page, close } = await openApp(browser, DOCS_URL);
  await expect(page.locator("[data-test=welcome]")).toContainText("No docs yet");
  await page.locator("[data-test=new-doc]").click();
  const dialog = page.locator("[data-test=create-dialog]");
  await dialog.locator("[name=title]").fill("Q3 launch plan");
  // Typing a name fills in the person and shows their email (AD-1).
  await dialog.locator("[name=people]").fill("Pri");
  await dialog.locator("[data-login=priya]").click();
  const her = dialog.locator("[data-person=priya]");
  await expect(her).toContainText("Priya Shah");
  await expect(her).toContainText("@");
  await expect(her.locator("select")).toHaveValue("editor");
  await expect(dialog.locator("[name=label]")).toHaveValue("internal");
  await shot(page, "docs-create");
  await dialog.locator("[data-test=dialog-submit]").click();
  await expect(dialog).toHaveCount(0);
  await expect(page.locator("[data-test=doc][data-ready=true]")).toBeVisible();
  docId = (await page.locator("[data-test=doc]").getAttribute("data-open")) ?? "";
  expect(docId).toMatch(/^cv-[0-9a-f]{32}$/);
  await expect(page.locator(`[data-test=mine] [data-doc="${docId}"]`)).toContainText("Q3 launch plan");
  await expect(page.locator("[data-test=title]")).toHaveText("Q3 launch plan");
  // What the person did in Docs was theirs: nothing asked.
  expect(glassctlJson<ApprovalView[]>("approvals")).toHaveLength(0);
  const made = docsCalls().filter((r) => r.event.action === "doc.create");
  expect(made.map((r) => r.event.decision?.tier)).toEqual(["auto"]);

  // Writing, with the Markdown-ish rules: a heading, a line, a checklist, a callout.
  const editor = page.locator("[data-test=editor]");
  await editor.click();
  await page.keyboard.type("# Goals");
  await page.keyboard.press("Enter");
  await page.keyboard.type("Ship the new onboarding to every pilot org by September.");
  await page.keyboard.press("Enter");
  await page.keyboard.type("[ ] Freeze the copy");
  await page.keyboard.press("Enter");
  await page.keyboard.type("Run the pilot");
  await page.keyboard.press("Enter");
  await page.keyboard.press("Enter");
  await page.keyboard.type("/warn");
  await expect(page.locator("[data-test=slash] [data-kind=warn]")).toBeVisible();
  await page.keyboard.press("Enter");
  await page.keyboard.type("Pricing is still being decided.");
  await expect(editor.locator("h1")).toHaveText("Goals");
  await expect(editor.locator(".task-item")).toHaveCount(2);
  await expect(editor.locator(".callout.warn")).toContainText("Pricing is still being decided.");
  await expect(page.locator("[data-test=sync]")).toHaveText("Saved", { timeout: 5000 });

  // Priya edits it: her role shows in Share.
  await page.locator("[data-test=share]").click();
  const share = page.locator("[data-test=share-dialog]");
  await expect(share.locator("[data-person=priya] select")).toHaveValue("editor");
  await expect(share.locator(`[data-person="${me}"]`)).toContainText("Owner");
  await shot(page, "docs-share");
  await share.getByRole("button", { name: "Cancel" }).click();
  await close();
});

test("dc3 what one page types shows on the other within the budget, with no audit record per key", async ({ browser }) => {
  const a = await openApp(browser, `${DOCS_URL}#${docId}`);
  const b = await openApp(browser, `${DOCS_URL}#${docId}`);
  await opened(a.page);
  await opened(b.page);
  await expect(b.page.locator("[data-test=editor] h1")).toHaveText("Goals");
  const before = docsCalls().length;

  const runs: number[] = [];
  for (const line of ["Week 1: internal.", "Week 2: the pilot orgs.", "Week 4: everyone."]) {
    // At the end of the last line, as a person clicking there would be.
    await a.page.locator("[data-test=editor] p").last().click();
    // The editor takes the click's caret on the browser's next selection
    // event; a key sent the same instant would act on the caret before it.
    await a.page.waitForTimeout(50);
    await a.page.keyboard.press("End");
    await a.page.keyboard.press("Enter");
    await a.page.keyboard.type(line, { delay: 15 });
    const typed = Date.now();
    await expect(b.page.locator("[data-test=editor]")).toContainText(line, { timeout: 10_000 });
    runs.push(Date.now() - typed);
    console.log(`docs: "${line}" on the other page ${runs.at(-1)} ms after its last key`);
  }
  // And back the other way.
  await b.page.locator("[data-test=editor] h1").click();
  await b.page.waitForTimeout(50);
  await b.page.keyboard.press("End");
  await b.page.keyboard.type(" for Q3");
  await expect(a.page.locator("[data-test=editor] h1")).toHaveText("Goals for Q3", { timeout: 5000 });

  const sorted = [...runs].sort((x, y) => x - y);
  const median = sorted[Math.floor(sorted.length / 2)]!;
  console.log(`docs: a typed line on the other page ${median} ms after the last key (runs ${runs.join(", ")})`);
  expect(median, "DC-3: an edit shows on another page within the budget").toBeLessThan(LIVE_BUDGET_MS);
  // About sixty keys typed; a handful of calls at most (the reads news brings), never one per key.
  expect(docsCalls().length - before).toBeLessThan(6);
  await shot(b.page, "docs-live");
  await a.close();
  await b.close();
});

test("dc6 the person's agent reads the doc as Markdown", async () => {
  const work = await agent("Read the Q3 launch plan");
  const read = ok(await work.call("doc_read", { doc: docId }))
    .content.map((c) => c.text)
    .join("\n");
  work.stop();
  expect(read).toContain("# Goals for Q3");
  expect(read).toContain("Ship the new onboarding to every pilot org by September.");
  expect(read).toMatch(/- \[ \] Freeze the copy/);
  expect(read).toContain("Pricing is still being decided.");
  expect(read).toContain("Week 4: everyone.");
});

test("dc3 a comment anchors at its word and stays on it while someone types before it", async ({ browser }) => {
  const a = await openApp(browser, `${DOCS_URL}#${docId}`);
  const b = await openApp(browser, `${DOCS_URL}#${docId}`);
  await opened(a.page);
  await opened(b.page);

  await select(a.page, "September");
  await a.page.locator("[data-test=bubble-comment]").click();
  const composer = a.page.locator("[data-test=composer]");
  await expect(composer.locator(".thread-quote")).toHaveText("September");
  await composer.locator("[name=draft]").fill("Is September realistic with the translations?");
  await composer.locator("[data-test=post-comment]").click();
  await expect(a.page.locator("[data-thread]")).toContainText("Is September realistic");
  await expect(a.page.locator("[data-test=editor] .comment-mark")).toHaveText("September");
  // The other page hears of it and highlights the same word.
  await expect(b.page.locator("[data-test=editor] .comment-mark")).toHaveText("September", { timeout: 10_000 });

  // Someone types before it on the other page; the comment stays on its word.
  await b.page.locator("[data-test=editor] p", { hasText: "Ship the new" }).click({ position: { x: 2, y: 8 } });
  await b.page.waitForTimeout(50);
  await b.page.keyboard.press("Home");
  await b.page.keyboard.type("Draft: ");
  await expect(a.page.locator("[data-test=editor] p", { hasText: "Draft: Ship" })).toBeVisible({ timeout: 5000 });
  await expect(a.page.locator("[data-test=editor] .comment-mark")).toHaveText("September");
  await expect(b.page.locator("[data-test=editor] .comment-mark")).toHaveText("September");
  await expect(a.page.locator("[data-thread] .thread-quote")).toContainText("September");
  // A reply, then the agent sees the open comment beside the Markdown.
  await a.page.locator("[data-thread]").getByRole("button", { name: "Reply" }).click();
  await a.page.locator("[name=reply]").fill("Yes, if the copy freezes this week.");
  await a.page.locator(".reply-box").getByRole("button", { name: "Reply" }).click();
  await expect(a.page.locator(".comment.reply")).toContainText("copy freezes");
  await shot(a.page, "docs-comments");
  const work = await agent("Check the Q3 plan's comments");
  const read = ok(await work.call("doc_read", { doc: docId }))
    .content.map((c) => c.text)
    .join("\n");
  work.stop();
  expect(read).toContain("Is September realistic with the translations?");

  // Resolved, it's no longer highlighted.
  await a.page.locator("[data-thread] [data-test=resolve]").first().click();
  await expect(a.page.locator("[data-test=editor] .comment-mark")).toHaveCount(0);
  expect(glassctlJson<ApprovalView[]>("approvals")).toHaveLength(0);
  await a.close();
  await b.close();
});

test("dc3 a named version is listed", async ({ browser }) => {
  const { page, close } = await openApp(browser, `${DOCS_URL}#${docId}`);
  await opened(page);
  await page.locator("[data-test=versions-toggle]").click();
  await page.locator("[name=version]").fill("Sent to Finance");
  await page.locator("[data-test=name-version]").getByRole("button", { name: "Save" }).click();
  await expect(page.locator("[data-test=versions] [data-version]")).toContainText("Sent to Finance");
  await close();
});

test("dc1 opening a doc draws it in under a second", async ({ browser }) => {
  const times: number[] = [];
  for (let run = 0; run < RUNS; run++) {
    const { page, close } = await openApp(browser, DOCS_URL);
    const row = page.locator(`[data-doc="${docId}"]`);
    await row.waitFor();
    const started = Date.now();
    await row.click();
    await expect(page.locator("[data-test=editor] h1")).toHaveText("Goals for Q3");
    times.push(Date.now() - started);
    if (run === RUNS - 1) await shot(page, "docs-doc");
    await close();
  }
  const sorted = [...times].sort((x, y) => x - y);
  const median = sorted[Math.floor(sorted.length / 2)]!;
  console.log(`docs: open a doc ${median} ms, click to the doc drawn (runs ${times.join(", ")})`);
  expect(median, "DC-1: a doc opens in under a second").toBeLessThan(OPEN_BUDGET_MS);
});
