// Calendar against the real system (UI-5, AU-10, NV-3): the week shows the
// hold an agent made and who made it, a new event runs at Notify, inviting
// people waits for approval on the desktop, and the loaded week reads
// offline.

import { mkdirSync } from "node:fs";
import { join } from "node:path";
import { type Page, expect, test } from "@playwright/test";

import { type ApprovalView, SHOTS, appUrl, audit, glassctl, glassctlJson, outcome } from "./env.ts";

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

test("ui5 nv3 invitations to people typed in wait for approval on the desktop, then go", async ({ page }) => {
  await openCalendar(page);
  await page.click("[data-test=new-event]");
  await page.fill("input[name=title]", "Renewal kickoff");
  await page.fill("input[name=start]", "11:00");
  await page.fill("input[name=end]", "11:30");
  const people = page.locator("input[name=attendees]");
  await people.pressSequentially("ops@vendor.example");
  await people.press("Escape");
  await page.click("[data-test=add-event]");
  await expect(page.locator("[data-test=notice]")).toContainText("waits for your approval on the desktop");
  let ask: ApprovalView | undefined;
  for (let i = 0; i < 100 && !ask; i++) {
    ask = glassctlJson<ApprovalView[]>("approvals").find((a) => a.agent === "calendar");
    if (!ask) await new Promise((r) => setTimeout(r, 100));
  }
  expect(ask, "the invitation asks on the desktop").toBeTruthy();
  expect(ask!.items.join(" ")).toContain("ops@vendor.example");
  await page.screenshot({ path: join(SHOTS, "calendar-invite-waiting.png") });
  glassctl("approve", ask!.id);
  await expect(page.locator("[data-test=notice]")).not.toContainText("waits for your approval", { timeout: 10_000 });
  const created = audit().find((r) => r.event.action === "calendar.create" && r.event.summary.includes("Renewal kickoff"));
  expect(created?.event.decision?.tier).toBe("notify");
  const invited = audit().filter((r) => r.event.action === "calendar.invite" && r.event.agent === "calendar");
  expect(invited.map(outcome)).toContain("completed");
  expect(invited.at(-1)?.event.decision?.tier).toBe("ask");
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
