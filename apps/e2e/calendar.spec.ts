// Calendar against the real system (UI-5, AU-10, NV-3): the week shows the
// hold an agent made and who made it, a new event runs at Notify, and the
// loaded week reads offline.

import { mkdirSync } from "node:fs";
import { join } from "node:path";
import { type Page, expect, test } from "@playwright/test";

import { SHOTS, appUrl, audit, outcome } from "./env.ts";

async function openCalendar(page: Page): Promise<void> {
  await page.goto(appUrl("calendar"));
  await expect(page.locator("html[data-ready=true]")).toHaveCount(1);
}

test.beforeAll(() => mkdirSync(SHOTS, { recursive: true }));

test("au10 the week shows the hold an agent made, by claude-code for the user", async ({ page }) => {
  await openCalendar(page);
  const hold = page.locator(".event.hold", { hasText: "Prep for Northwind renewal" });
  await expect(hold).toHaveCount(1);
  await expect(hold).toContainText("by claude-code");
  await expect(page.locator(".event", { hasText: "Northwind renewal call" })).toHaveClass(/invited/);
  await hold.click();
  const detail = page.locator("[data-test=detail]");
  await expect(detail).toContainText("Hold · tentative");
  await expect(detail.locator(".attribution")).toHaveText(/^by claude-code for \S+$/);
  await page.screenshot({ path: join(SHOTS, "calendar-week-agent-hold.png") });
});

test("ui5 a new event on the user's own calendar runs at notify", async ({ page }) => {
  await openCalendar(page);
  await page.click("[data-test=new-event]");
  await page.fill("input[name=title]", "Contract review");
  await page.fill("input[name=start]", "16:30");
  await page.fill("input[name=end]", "17:15");
  await page.fill("input[name=location]", "Room 4B");
  await page.click("[data-test=add-event]");
  await expect(page.locator("[data-test=notice]")).toContainText("Added");
  const created = page.locator(".event", { hasText: "Contract review" });
  await expect(created).toHaveCount(1);
  await expect(created).not.toContainText("by ");
  const record = audit().find((r) => r.event.action === "calendar.create" && r.event.agent === "calendar");
  expect(record && outcome(record)).toBe("completed");
  expect(record?.event.decision?.tier).toBe("notify");
});

test("ui5 the loaded week reads offline", async ({ page, context }) => {
  await openCalendar(page);
  await page.evaluate(() => navigator.serviceWorker.ready);
  await context.setOffline(true);
  await page.reload();
  await expect(page.locator("html[data-ready=true]")).toHaveCount(1);
  await expect(page.locator("[data-test=offline]")).toContainText("Offline");
  await expect(page.locator(".event.hold", { hasText: "Prep for Northwind renewal" })).toHaveCount(1);
  await expect(page.locator("[data-test=new-event]")).toBeDisabled();
  await context.setOffline(false);
});
