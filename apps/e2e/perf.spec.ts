// UI-7's budgets for web apps: cold launch under 1 s and typical memory
// under 150 MB per app (docs/dev/performance.md). The test fails when an
// app exceeds either.
//
// Cold launch: a fresh browser context (no cache, no service worker, no
// stored data) from navigation to the app drawn with its data (the
// `glass:ready` mark), median of three. Memory: the resident memory of the
// app's renderer process (its page and service worker) after it has loaded
// and been used; the browser and GPU processes are shared by every app.

import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { type Browser, type Page, expect, test } from "@playwright/test";

import { REPO, appUrl } from "./env.ts";

const LAUNCH_BUDGET_MS = 1000;
const MEMORY_BUDGET_MB = 150;
const RUNS = 3;

interface Measured {
  app: string;
  launchesMs: number[];
  launchMs: number;
  memoryMb: number;
}

const results: Measured[] = [];

async function coldLaunch(browser: Browser, app: string): Promise<{ ms: number; page: Page; close: () => Promise<void> }> {
  const context = await browser.newContext({ viewport: { width: 1280, height: 800 } });
  const page = await context.newPage();
  const started = Date.now();
  await page.goto(appUrl(app));
  await page.waitForSelector("html[data-ready=true]", { state: "attached" });
  return { ms: Date.now() - started, page, close: () => context.close() };
}

/** Resident memory of each renderer process, in MB. */
async function rendererMemory(browser: Browser): Promise<number[]> {
  const cdp = await browser.newBrowserCDPSession();
  const info = (await cdp.send("SystemInfo.getProcessInfo")) as { processInfo: { type: string; id: number }[] };
  await cdp.detach();
  return info.processInfo
    .filter((p) => p.type === "renderer")
    .map((p) => {
      const status = readFileSync(`/proc/${p.id}/status`, "utf8");
      const kb = Number(/VmRSS:\s+(\d+)/.exec(status)?.[1] ?? 0);
      return kb / 1024;
    });
}

for (const [app, use] of [
  ["mail", async (page: Page) => page.locator(".row").first().click()],
  ["calendar", async (page: Page) => page.locator(".event").first().click()],
  ["store", async (page: Page) => page.locator("input.search").fill("contracts")],
  // This suite's computer is unmanaged, so Chat says it has no chat; the
  // console suite times it with conversations (chat.e2e.ts).
  ["chat", async (page: Page) => page.getByRole("button", { name: "New message" }).first().click()],
  // Meet opens on its lobby here; the console suite times joining and a
  // call's memory with two people (meetings.e2e.ts).
  ["meet", async (page: Page) => page.locator("[data-test=join]").hover()],
  // Unmanaged here too, so Artifacts says the shared space isn't ready; the
  // console suite times opening one (artifacts.e2e.ts).
  ["artifacts", async (page: Page) => page.locator("[data-test=share]").click()],
] as const) {
  test(`ui7 ${app} launches cold in under 1 s and uses under 150 MB`, async ({ browser }) => {
    const launches: number[] = [];
    let memoryMb = 0;
    for (let run = 0; run < RUNS; run++) {
      const { ms, page, close } = await coldLaunch(browser, app);
      launches.push(ms);
      if (run === RUNS - 1) {
        await use(page);
        await page.waitForTimeout(300);
        // One app open: the largest renderer is its own.
        memoryMb = Math.max(...(await rendererMemory(browser)));
      }
      await close();
    }
    const sorted = [...launches].sort((a, b) => a - b);
    const launchMs = sorted[Math.floor(sorted.length / 2)] ?? Number.POSITIVE_INFINITY;
    results.push({ app, launchesMs: launches, launchMs, memoryMb: Math.round(memoryMb) });
    console.log(`${app}: cold launch ${launchMs} ms (runs ${launches.join(", ")}), renderer ${memoryMb.toFixed(0)} MB`);
    expect(launchMs, `${app} cold launch (UI-7)`).toBeLessThan(LAUNCH_BUDGET_MS);
    expect(memoryMb, `${app} memory (UI-7)`).toBeGreaterThan(0);
    expect(memoryMb, `${app} memory (UI-7)`).toBeLessThan(MEMORY_BUDGET_MB);
  });
}

test.afterAll(() => {
  const dir = join(REPO, "apps/e2e/results");
  mkdirSync(dir, { recursive: true });
  writeFileSync(join(dir, "perf.json"), `${JSON.stringify(results, null, 2)}\n`);
});
