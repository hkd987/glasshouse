// Routines (DA-4, NV-21, ADR 0047): the desktop agent, on the scripted
// model, drafts the same email twice; Routines offers to keep it with the
// code this computer wrote; kept, it runs with no model, changes when it
// runs, pauses, and goes. Every step is the person's own, by Routines for
// them, on the record.

import { mkdirSync } from "node:fs";
import { join } from "node:path";
import { expect, test } from "@playwright/test";

import { SHOTS, appUrl, audit, glassctl, glassctlJson } from "./env.ts";

const PROMPT = `Draft it. Use mail_draft {"to": ["ana@example.com"], "subject": "Weekly numbers", "body": "Here are this week's numbers."}`;

/** The desktop agent does the request, and is done. */
function draftIt(): string {
  const task = glassctlJson<{ id: string }>("run", "desktop-agent", PROMPT, "--yes").id;
  glassctl("wait", task, "--timeout", "60");
  return task;
}

test.beforeAll(() => mkdirSync(SHOTS, { recursive: true }));

test("adr0047 a request done twice the same way is kept, run with no model, rescheduled, paused and removed in Routines", async ({ page }) => {
  draftIt();
  const second = draftIt();

  await page.goto(appUrl("routines"));
  await expect(page.locator("html[data-ready=true]")).toHaveCount(1);
  const repeat = page.locator(`[data-repeat="${second}"]`);
  await expect(repeat).toContainText("Done the same way 2 times");
  await repeat.locator("summary").click();
  await expect(repeat.locator("pre.code")).toContainText('call("mail_draft"');
  await page.screenshot({ path: join(SHOTS, "routines-repeat.png") });

  // Kept, every Friday at 17:00: the person's own act, no ask.
  await repeat.locator("[data-test=start-keep]").click();
  await repeat.locator("input[name=name]").fill("Weekly numbers draft");
  await repeat.locator("select[name=kind]").selectOption("weekly");
  await repeat.locator("select[name=day]").selectOption("fri");
  // Typed a key at a time, as a person does: the field keeps what's typed.
  await repeat.locator("input[name=at]").focus();
  await page.keyboard.type("0500P");
  await repeat.locator("[data-test=keep]").click();
  const card = page.locator('[data-routine="weekly-numbers-draft"]');
  await expect(card.locator("[data-test=when]")).toContainText("Runs every Friday at 17:00 · next");
  await expect(page.locator("[data-test=repeats]")).toHaveCount(0);
  expect(glassctlJson<unknown[]>("approvals")).toEqual([]);

  // Run now: a task of its own, done with the script alone.
  await card.locator("[data-test=run]").click();
  await expect(card.locator("[data-test=last-run]")).toContainText("Ran with no model", { timeout: 60_000 });
  await page.screenshot({ path: join(SHOTS, "routines-list.png") });

  // Opened: what it reaches, the code, its runs.
  await card.locator("a.name").click();
  await expect(page.locator("[data-test=code]")).toContainText('call("mail_draft"');
  await expect(page.locator("[data-test=runs] tr")).toHaveCount(1);
  await page.screenshot({ path: join(SHOTS, "routines-detail.png"), fullPage: true });

  // Every weekday at 09:00 now: its next version, the same code.
  await page.click("[data-test=change-schedule]");
  await page.locator("select[name=kind]").selectOption("weekdays");
  await page.locator("input[name=at]").fill("09:00");
  await page.click("[data-test=save-schedule]");
  await expect(page.locator("[data-test=when]")).toContainText("Runs every weekday at 09:00");
  await expect(page.locator("[data-test=notice]")).toContainText("version 2");

  // Paused, then resumed.
  await page.click("[data-test=pause]");
  await expect(page.locator("[data-test=when]")).toContainText("Paused");
  await page.click("[data-test=pause]");
  await expect(page.locator("[data-test=when]")).toContainText("Runs every weekday at 09:00 · next");

  // Removed, after asking.
  await page.click("[data-test=remove]");
  await page.click("[data-test=remove-yes]");
  await expect(page.locator("[data-test=empty]")).toBeVisible();

  // On the record, by Routines for the person.
  const byRoutines = audit().filter((r) => r.event.agent === "routines" && r.event.outcome?.status === "completed");
  for (const action of ["routines.keep", "routines.run", "routines.schedule", "routines.pause", "routines.delete"]) {
    expect(byRoutines.some((r) => r.event.action === action), action).toBe(true);
  }
});
