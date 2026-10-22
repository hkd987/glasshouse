// IT's side of tools/dev/real-remix.sh: the console's review page for an app a person shared,
// IT approves it for everyone (available), the computer gets it, the Store lists it, the person
// installs or updates it and it opens. Screenshots of each step.
// usage: node real-remix-review.mjs <catalog-id> <shots-dir> <glassd-dev-dir>
import { mkdirSync, readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { userInfo } from "node:os";
import { join } from "node:path";

const require = createRequire(join(process.cwd(), "package.json"));
const { chromium } = require("@playwright/test");

const [app, shots, dev] = process.argv.slice(2);
const CONSOLE = "http://127.0.0.1:7410/";
const STORE = "http://store.localhost:7411/";
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const audit = () =>
  readFileSync(join(dev, "state/audit/audit.log"), "utf8")
    .split("\n")
    .filter(Boolean)
    .map((line) => JSON.parse(line).body);
mkdirSync(shots, { recursive: true });
const shot = (page, name) => page.screenshot({ path: join(shots, `${name}.png`) });

const browser = await chromium.launch();
try {
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  const page = await context.newPage();
  page.on("pageerror", (e) => console.error(`console page error: ${e.message}`));
  await page.goto(CONSOLE);
  await page.click(`button[data-user="${userInfo().username}"]`);
  await page.waitForSelector("html[data-ready=true][data-page=policy]");
  await page.goto(`${CONSOLE}#/catalog/${app}`);
  await page.waitForSelector("html[data-ready=true][data-page=catalog]", { state: "attached" });
  await page.locator("[data-test=made-by]").waitFor();
  const name = await page.locator("h1").innerText();
  const version = (await page.locator("[data-test=made-by]").innerText()).match(/v(\d+)$/)?.[1] ?? "?";
  console.log(`${name}: ${await page.locator(".crumbs").innerText()}`);
  console.log(`  ${await page.locator("[data-test=made-by]").innerText()}`);
  console.log(`  ${await page.locator("[data-test=beyond] h2").innerText()}`);
  const failed = await page.locator("[data-test=checks] [data-test=check][data-ok=false]").count();
  console.log(`  checks: ${await page.locator("[data-test=checks] [data-test=check]").count()}, failed ${failed}`);
  console.log(`  ${(await page.locator("[data-test=changes] p").first().innerText().catch(() => "")).trim()}`);
  await shot(page, `review-v${version}`);
  for (const file of await page.locator("[data-test=changes] details").all()) {
    if ((await file.getAttribute("open")) === null) await file.locator("summary").click();
  }
  await page.locator("[data-test=changes]").scrollIntoViewIfNeeded();
  await shot(page, `review-v${version}-changes`);
  if (!(await page.locator(".crumbs").innerText()).includes("Waiting for review")) {
    throw new Error(`${name} isn't waiting for review`);
  }

  const before = audit().length;
  await page.locator("select[data-test=mode]").selectOption("available");
  await page.click("button[data-test=approve]");
  const decided = Date.now();
  await page.locator(".notice", { hasText: "Approved. Catalog v" }).waitFor();
  await page.evaluate(() => document.querySelector("main.content")?.scrollTo(0, 0));
  await shot(page, `approved-v${version}`);
  const arrived = new RegExp(`^App ${app} ${version} from the org's catalog installed`);
  while (!audit().slice(before).some((r) => arrived.test(r.event.summary ?? ""))) {
    if (Date.now() - decided > 20_000) throw new Error("not on the computer 20 s after IT approved it");
    await sleep(50);
  }
  console.log(`approved to on the computer: ${Date.now() - decided} ms`);

  const store = await context.newPage();
  const card = store.locator(`article[data-id=${app}]`);
  for (let tries = 0; ; tries++) {
    await store.goto(STORE);
    await store.waitForSelector("html[data-ready=true]");
    if ((await card.count()) > 0) break;
    if (tries > 40) throw new Error("the Store doesn't list it");
    await sleep(250);
  }
  console.log(`Store: ${(await card.innerText()).replace(/\n+/g, " · ")}`);
  await card.scrollIntoViewIfNeeded();
  await shot(store, `store-v${version}`);
  const install = card.locator("button", { hasText: /^(Install|Update)$/ });
  if ((await install.count()) > 0) {
    await install.click();
    await store.locator(".banner.done").waitFor();
    await shot(store, `store-v${version}-installed`);
  }

  const opened = await context.newPage();
  opened.on("pageerror", (e) => console.error(`app page error: ${e.message}`));
  await opened.goto(`http://${app}.localhost:7411/`);
  await opened.waitForSelector("html[data-ready=true]");
  await sleep(500);
  console.log(`opened: ${await opened.title()}`);
  await shot(opened, `opened-v${version}`);
  console.log(`screenshots in ${shots}`);
} finally {
  await browser.close();
}
