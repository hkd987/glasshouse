// Screenshots of the live web pages for the walkthrough video (1440x900,
// as the console's own tests take them): the admin console and Glasshouse
// Agents, each driven as a person would. tools/video/shots.sh runs it
// against what it started.
//
// usage: node tools/video/capture.mjs console <console url> <admin> <out dir>
//        node tools/video/capture.mjs agents <signed-in url> <out dir>

import { mkdirSync } from "node:fs";
import { createRequire } from "node:module";
import { join } from "node:path";

// The machine's Playwright (NODE_PATH), or the apps workspace's.
const require = createRequire(import.meta.url);
let playwright;
try {
  playwright = require("playwright");
} catch {
  playwright = createRequire(join(process.cwd(), "apps", "x.js"))("@playwright/test");
}
const { chromium } = playwright;

const [part, ...rest] = process.argv.slice(2);
const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
page.on("pageerror", (e) => console.error(`page error: ${e.message}`));

async function settle() {
  await page.waitForLoadState("networkidle").catch(() => {});
  await page.waitForTimeout(400);
}

if (part === "console") {
  const [url, admin, out] = rest;
  mkdirSync(out, { recursive: true });
  await page.goto(url);
  await page.click(`button[data-user="${admin}"]`);
  await page.waitForSelector("html[data-ready=true][data-page=policy]");
  await page.goto(`${url}#/policy/everyone`);
  await page.waitForSelector("html[data-ready=true][data-page=policy]", { state: "attached" });
  await settle();
  await page.screenshot({ path: join(out, "console-tiers.png") });
  // IT turns Calendar off for the org.
  const calendar = page.locator("[data-test=app-calendar] select");
  await calendar.scrollIntoViewIfNeeded();
  await calendar.selectOption("off");
  await page.waitForTimeout(600);
  await page.locator("[data-test=app-calendar]").scrollIntoViewIfNeeded();
  await page.evaluate(() => {
    const row = document.querySelector("[data-test=app-calendar]");
    row?.closest(".card")?.scrollIntoView({ block: "center" });
  });
  await page.waitForTimeout(300);
  await page.screenshot({ path: join(out, "console-apps-off.png") });
  await page.click("button[data-test=preview]");
  await page.waitForSelector("[data-test=changes]");
  await page.evaluate(() => document.querySelector("main.content")?.scrollTo(0, 0));
  await page.waitForTimeout(300);
  await page.screenshot({ path: join(out, "console-impact.png") });
} else if (part === "agents") {
  const [url, out] = rest;
  mkdirSync(out, { recursive: true });
  await page.goto(url);
  await settle();
  await page.screenshot({ path: join(out, "agents-list.png") });
  // The agent waiting on its person, from the top of its page: the page
  // scrolls to the latest message, so it's taller here and scrolled back.
  const waiting = page.locator("a[href^='/agents/']", { hasText: "Needs you" }).first();
  await ((await waiting.count()) ? waiting : page.locator("a[href^='/agents/']").first()).click();
  await settle();
  await page.setViewportSize({ width: 1440, height: 1200 });
  await page.waitForTimeout(800);
  await page.evaluate(() => {
    window.scrollTo(0, 0);
    for (const el of document.querySelectorAll("*")) if (el.scrollTop > 0) el.scrollTop = 0;
  });
  await page.waitForTimeout(300);
  await page.screenshot({ path: join(out, "agents-detail.png") });
} else {
  console.error("usage: capture.mjs console|agents ...");
  process.exit(2);
}
await browser.close();
