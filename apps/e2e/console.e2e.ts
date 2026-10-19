// The admin console end to end (ADR 0023): IT signs in, and against the
// 500-device, 50-item fixture every page is interactive in under 1 s
// (AD-4); a policy change previewed and published to the pilot ring
// reaches the dev-host device in under 5 s (AD-3, DV-1); an item IT
// approves reaches catalog.search on the device (DV-11, GA-2). The test
// fails when a budget is exceeded. Screenshots are 1440x900.

import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { userInfo } from "node:os";
import { join } from "node:path";
import { type Browser, type BrowserContext, type Page, expect, test } from "@playwright/test";

import { CONSOLE_SHOTS, CONSOLE_URL, DEVICE } from "./console-env.ts";
import { DEV, audit, glassctl, glassctlJson, mcp, outcome } from "./env.ts";

/** AD-4: a page IT opens is interactive within this. */
const PAGE_BUDGET_MS = 1000;
/** AD-3: a published change is in force on an online pilot device within this. */
const PROPAGATION_BUDGET_MS = 5000;
const RUNS = 3;
const ADMIN = userInfo().username;

test.describe.configure({ mode: "serial" });

const measured: Record<string, unknown> = {};
let signedIn: Awaited<ReturnType<BrowserContext["storageState"]>> | null = null;

async function session(browser: Browser): Promise<{ page: Page; close: () => Promise<void> }> {
  const context = await browser.newContext(signedIn ? { storageState: signedIn } : {});
  const page = await context.newPage();
  page.on("pageerror", (e) => console.error(`page error: ${e.message}`));
  if (!signedIn) {
    await page.goto(CONSOLE_URL);
    await page.click(`button[data-user="${ADMIN}"]`);
    await page.waitForSelector("html[data-ready=true][data-page=policy]");
    signedIn = await context.storageState();
  }
  return { page, close: () => context.close() };
}

async function open(page: Page, hash: string, section: string): Promise<number> {
  const started = Date.now();
  await page.goto(`${CONSOLE_URL}${hash}`);
  await page.waitForSelector(`html[data-ready=true][data-page=${section}]`, { state: "attached" });
  return Date.now() - started;
}

async function shot(page: Page, name: string): Promise<void> {
  mkdirSync(CONSOLE_SHOTS, { recursive: true });
  await page.screenshot({ path: join(CONSOLE_SHOTS, `${name}.png`) });
}

function status(): { policy: string } {
  return glassctlJson<{ policy: string }>("status");
}

function version(source: string): number {
  return Number(/managed v(\d+)/.exec(source)?.[1] ?? 0);
}

test.afterAll(() => {
  writeFileSync(join(DEV, "..", "measured.json"), JSON.stringify(measured, null, 2));
  console.log(JSON.stringify(measured, null, 2));
});

test("ad4 every page is interactive in under 1 s with 500 devices and 50 items", async ({ browser }) => {
  await (await session(browser)).close();
  const pages: [string, string, string][] = [
    ["policy", "#/policy/ap-ops", "policy"],
    ["catalog", "#/catalog", "catalog"],
    ["item", "#/catalog/contract-review", "catalog"],
    ["devices", "#/devices", "devices"],
  ];
  for (const [name, hash, section] of pages) {
    const runs: number[] = [];
    for (let run = 0; run < RUNS; run++) {
      // A fresh context each time: nothing cached but the session.
      const { page, close } = await session(browser);
      runs.push(await open(page, hash, section));
      if (name === "devices") await expect(page.locator("tr[data-device]")).toHaveCount(501);
      await close();
    }
    const median = [...runs].sort((a, b) => a - b)[Math.floor(RUNS / 2)] ?? Number.POSITIVE_INFINITY;
    measured[`page_${name}_ms`] = { median, runs };
    expect(median, `${name} page interactive (AD-4)`).toBeLessThan(PAGE_BUDGET_MS);
  }
});

test("ad3 dv1 a change previewed and published to the pilot ring reaches the device", async ({ browser }) => {
  const { page, close } = await session(browser);
  const before = version(status().policy);
  expect(before).toBeGreaterThan(0);

  // AD-11: IT puts this device in the pilot ring.
  await open(page, "#/devices", "devices");
  const row = page.locator(`tr[data-device="${DEVICE}"]`);
  await expect(row).toHaveCount(1);
  await row.locator("select").selectOption("pilot");
  await expect(page.locator(".notice")).toContainText("pilot ring");

  // AD-2: IT admins' sends never go out; AD-3: preview, then publish.
  await open(page, "#/policy/it-admins", "policy");
  await page.click("tr[data-class=send] .seg.never");
  await expect(page.locator("tr[data-class=send] .seg.never.on")).toHaveCount(1);
  await page.click("button[data-test=preview]");
  const impact = page.locator("[data-test=impact]");
  await expect(impact).toContainText(/This change affects \d+ users? and \d+ agents?\./);
  await shot(page, "policy-change-previewed");

  await page.click("button[data-test=publish]");
  const published = Date.now();
  await expect(page.locator(".notice")).toContainText("to the pilot ring");
  let source = status().policy;
  while (version(source) <= before && Date.now() - published < 2 * PROPAGATION_BUDGET_MS) {
    await page.waitForTimeout(20);
    source = status().policy;
  }
  const took = Date.now() - published;
  measured.propagation_ms = took;
  expect(version(source), source).toBe(before + 1);
  expect(took, "publish to in force on the pilot device (AD-3)").toBeLessThan(PROPAGATION_BUDGET_MS);

  // The device enforces it, kept the previous version, and audited it.
  const policy = readFileSync(join(DEV, "policy.toml"), "utf8");
  expect(policy).toContain('"mail.send" = "never"');
  const loaded = audit().filter((r) => r.event.kind === "policy_loaded" && r.event.summary.includes(`v${before + 1}`));
  expect(loaded.length).toBe(1);

  // Role-based delivery: IT's admins get the admin console in their
  // profile, pointing at the console's servers.
  const apps = glassctlJson<{ id: string; kind: string; url: string | null }[]>("apps");
  expect(apps.find((a) => a.id === "console")?.url).toBe(CONSOLE_URL.replace(/\/$/, ""));
  await close();
});

test("dv11 ga2 an item IT approves reaches catalog.search on the device", async ({ browser }) => {
  const { page, close } = await session(browser);
  await open(page, "#/catalog/contract-review", "catalog");
  await expect(page.locator("h1")).toHaveText("Contract Review agent");
  await expect(page.locator(".crumbs")).toContainText("Waiting for review");
  await shot(page, "catalog-awaiting-approval");

  await page.locator("select[data-test=group]").selectOption("everyone");
  await page.locator("select[data-test=mode]").selectOption("available");
  await page.click("button[data-test=approve]");
  const notice = page.locator(".notice");
  await expect(notice).toContainText("Approved. Catalog v");
  const catalog = Number(/Catalog v(\d+)/.exec((await notice.textContent()) ?? "")?.[1]);

  const approved = Date.now();
  while (!audit().some((r) => r.event.kind === "catalog_loaded" && r.event.summary.includes(`Catalog v${catalog} `))) {
    expect(Date.now() - approved, "catalog on the device").toBeLessThan(PROPAGATION_BUDGET_MS);
    await page.waitForTimeout(20);
  }
  measured.catalog_propagation_ms = Date.now() - approved;

  // GA-2: the user's own harness looks in the catalog before building.
  const task = glassctlJson<{ id: string; driver_socket: string }>(
    "run",
    "claude-code",
    "Build me something to redline contracts",
    "--title",
    "Contract redlines",
    "--yes",
  );
  const [found] = (await mcp(task.driver_socket, [
    { name: "catalog_search", arguments: { need: "review contracts and draft a redline" } },
  ])) as { isError?: boolean; content: { text: string }[] }[];
  glassctl("stop", task.id);
  expect(found?.isError ?? false).toBe(false);
  const text = found?.content.map((c) => c.text).join("\n") ?? "";
  expect(text).toContain("Contract Review agent");
  expect(text).toContain("they can install it themselves");
  const call = audit().find((r) => r.event.action === "catalog.search");
  expect(call?.event.decision?.tier).toBe("auto");
  expect(outcome(call!)).toBe("completed");
  await close();
});

test("ad4 the fleet shows the device on the new versions", async ({ browser }) => {
  const { page, close } = await session(browser);
  const policy = version(status().policy);
  await expect(async () => {
    await open(page, "#/devices", "devices");
    await expect(page.locator(`tr[data-device="${DEVICE}"]`)).toContainText(`v${policy}`, { timeout: 500 });
  }).toPass({ timeout: 10_000 });
  await page.locator(`tr[data-device="${DEVICE}"]`).scrollIntoViewIfNeeded();
  await page.evaluate(() => window.scrollTo(0, 0));
  await shot(page, "fleet");
  const text = (await page.locator("body").innerText()).toLowerCase();
  expect(text).not.toContain("freebsd");
  await close();
});
