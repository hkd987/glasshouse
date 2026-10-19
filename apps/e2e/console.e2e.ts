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

import { CALENDAR_URL, CONSOLE_SHOTS, CONSOLE_URL, DEVICE, MAIL_URL, STORE_URL } from "./console-env.ts";
import { DEV, audit, glassctl, glassctlJson, mcp, outcome } from "./env.ts";

/** AD-4: a page IT opens is interactive within this. */
const PAGE_BUDGET_MS = 1000;
/** AD-3: a published change is in force on an online pilot device within this. */
const PROPAGATION_BUDGET_MS = 5000;
/** A keystroke in an address field to the person suggested, debounce included (AD-1). */
const SUGGEST_BUDGET_MS = 500;
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

test("dv11 the Store shows what IT made available and installs it", async ({ browser }) => {
  const page = await browser.newPage();
  // Approved for everyone, available, by the test before.
  await expect(async () => {
    await page.goto(STORE_URL);
    await expect(page.locator("html[data-ready=true]")).toHaveCount(1, { timeout: 2000 });
    await expect(page.locator("article[data-id=contract-review]")).toHaveCount(1, { timeout: 500 });
  }).toPass({ timeout: 20_000 });
  const card = page.locator("article[data-id=contract-review]");
  await expect(card).toContainText("Contract Review agent");
  await expect(card).toContainText("Can do");
  await expect(card.locator("button")).toHaveText("Install");
  await page.screenshot({ path: join(CONSOLE_SHOTS, "store-available.png") });

  await card.locator("button").click();
  await expect(page.locator(".banner.done")).toContainText("Installed Contract Review agent");
  await expect(page.locator("section", { hasText: "Yours" }).locator("article[data-id=contract-review]")).toHaveCount(1);
  await page.screenshot({ path: join(CONSOLE_SHOTS, "store-installed.png") });
  const call = audit().find((r) => r.event.action === "catalog.install");
  expect(call?.event.decision?.tier).toBe("notify");
  expect(outcome(call!)).toBe("completed");

  await page.locator("article[data-id=contract-review] button", { hasText: "Remove" }).click();
  await expect(page.locator(".banner.done")).toContainText("Removed Contract Review agent");
  await page.close();
});

test("ad1 ui5 Mail and Calendar suggest people from the org's directory as you type", async ({ browser }) => {
  // The console signed the directory when it started; the device applied it.
  const started = Date.now();
  while (!audit().some((r) => r.event.kind === "directory_loaded")) {
    expect(Date.now() - started, "directory on the device").toBeLessThan(PROPAGATION_BUDGET_MS * 4);
    await new Promise((r) => setTimeout(r, 20));
  }
  const page = await browser.newPage();
  await page.goto(MAIL_URL);
  await expect(page.locator("html[data-ready=true]")).toHaveCount(1);
  await page.click("[data-test=compose]");
  const to = page.locator("input[name=to]");
  await to.pressSequentially("pr");
  const typed = Date.now();
  await to.press("i");
  const priya = page.locator(".suggestion", { hasText: "priya@acme.example" });
  await expect(priya).toHaveCount(1);
  measured.people_suggest_ms = Date.now() - typed;
  expect(measured.people_suggest_ms, "a keystroke to the person suggested").toBeLessThan(SUGGEST_BUDGET_MS);
  await expect(priya).toContainText("Priya Shah");
  await expect(priya).toContainText("Counsel");
  await page.screenshot({ path: join(CONSOLE_SHOTS, "mail-people-suggested.png") });
  await to.press("Enter");
  await expect(to).toHaveValue("priya@acme.example, ");
  const call = audit().find((r) => r.event.action === "people.search" && r.event.agent === "mail");
  expect(call?.event.decision?.tier).toBe("auto");
  expect(outcome(call!)).toBe("completed");

  await page.goto(CALENDAR_URL);
  await expect(page.locator("html[data-ready=true]")).toHaveCount(1);
  await page.click("[data-test=new-event]");
  const people = page.locator("input[name=attendees]");
  await people.pressSequentially("dan");
  await expect(page.locator(".suggestion", { hasText: "dana@acme.example" })).toHaveCount(1);
  await page.screenshot({ path: join(CONSOLE_SHOTS, "calendar-people-suggested.png") });
  await people.press("Tab");
  await expect(people).toHaveValue("dana@acme.example, ");
  await page.close();
});

test("nv3 ad2 the mail service IT sets in the console shows in Mail on the device", async ({ browser }) => {
  const { page, close } = await session(browser);
  const before = version(status().policy);
  await open(page, "#/mail", "mail");
  await page.locator("select[name=service]").selectOption("microsoft-365");
  await page.fill("input[name=tenant]", "acme.example");
  await page.fill("input[name=address_template]", "{login}@acme.example");
  await page.click("button[data-test=save-mail]");
  await expect(page.locator("[data-test=impact]")).toContainText("Everyone: mail through Microsoft 365 (acme.example)");
  await shot(page, "mail-settings-previewed");
  await page.click("button[data-test=publish]");
  await expect(page.locator(".notice")).toContainText("to the pilot ring");
  await shot(page, "mail-settings-published");
  const published = Date.now();
  while (version(status().policy) <= before) {
    expect(Date.now() - published, "mail settings on the device").toBeLessThan(PROPAGATION_BUDGET_MS);
    await page.waitForTimeout(20);
  }
  await close();

  const mail = await browser.newPage();
  await mail.goto(MAIL_URL);
  await expect(mail.locator("html[data-ready=true]")).toHaveCount(1);
  const account = mail.locator("[data-test=account]");
  await expect(account).toContainText("Microsoft 365 · acme.example · not connected yet");
  await expect(account).toContainText("@acme.example");
  await mail.click("[data-test=compose]");
  await expect(mail.locator("[data-test=from]")).toContainText("@acme.example>");
  await mail.screenshot({ path: join(CONSOLE_SHOTS, "mail-account-from-console.png") });
  const call = audit().find((r) => r.event.action === "mail.account" && r.event.agent === "mail");
  expect(call?.event.decision?.tier).toBe("auto");
  await mail.close();
});

/** Signs a support person in, in a context of their own. */
async function supportSession(browser: Browser, user: string): Promise<{ page: Page; close: () => Promise<void> }> {
  const context = await browser.newContext();
  const page = await context.newPage();
  page.on("pageerror", (e) => console.error(`page error: ${e.message}`));
  await page.goto(CONSOLE_URL);
  await page.click(`button[data-user="${user}"]`);
  await page.waitForSelector("html[data-ready=true][data-page=support]");
  return { page, close: () => context.close() };
}

test("cf80 a help desk person sees Support, not Policies", async ({ browser }) => {
  const { page, close } = await supportSession(browser, "jordan");
  const nav = page.locator("nav.sidebar");
  await expect(nav).toContainText("Support");
  await expect(nav).not.toContainText("Policies");
  await expect(page.locator(".topbar")).toContainText("Jordan Lee · IT Help Desk");
  await expect(page.locator("[data-tool=view]")).toContainText("Yours");
  await expect(page.locator("[data-tool=install_apps]")).toContainText("Needs Desktop Support");
  await expect(page.locator("[data-tool=isolate]")).toContainText("Needs Security");
  await page.fill("input[name=search]", DEVICE);
  await expect(page.locator(`tr[data-device="${DEVICE}"]`)).toHaveCount(1);
  await shot(page, "support-help-desk");

  // The console refuses the rest, whatever the page shows (ADR 0028).
  for (const [method, path] of [
    ["GET", "/api/policy"],
    ["POST", "/api/policy/publish"],
    ["GET", "/api/fleet"],
    ["GET", "/api/roles"],
  ] as const) {
    const r = await page.request.fetch(new URL(path, CONSOLE_URL).href, { method, data: method === "POST" ? {} : undefined });
    expect(r.status(), `${method} ${path}`).toBe(403);
  }
  // A page their session doesn't open lands on Support.
  expect(await open(page, "#/policy/ap-ops", "support")).toBeLessThan(PAGE_BUDGET_MS);
  await close();
});

test("cf81 IT gives the help desk a tool and it is theirs once published", async ({ browser }) => {
  const { page, close } = await session(browser);
  await open(page, "#/remote-help", "remote-help");
  const desk = page.locator("[data-role=help-desk]");
  await expect(desk).toContainText("Held by Jordan Lee");
  await desk.locator("input[name=tool-install_apps]").check();
  await desk.locator("[data-test=save-role]").click();
  await expect(page.locator("[data-test=impact]")).toContainText("Remote help: IT Help Desk may now install approved apps");
  await shot(page, "remote-help-previewed");
  await page.click("[data-test=publish]");
  await expect(page.locator(".notice")).toContainText("Published");
  await close();

  const jordan = await supportSession(browser, "jordan");
  await expect(jordan.page.locator("[data-tool=install_apps]")).toContainText("Yours");
  await jordan.close();
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
