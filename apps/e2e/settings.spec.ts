// Settings on the computer (UX-1): what the person hid from Ctrl Space
// (DK-1) is listed under Search, by the names the computer offers the apps
// under (an id for one it no longer has), and comes back one at a time.

import { mkdirSync } from "node:fs";
import { join } from "node:path";
import { expect, test } from "@playwright/test";

import { SHOTS, appUrl, audit, control } from "./env.ts";

interface SettingsView {
  settings: { id: string; value: unknown }[];
}

const HIDDEN = "search.hidden_apps";
const hidden = async () => (await control<SettingsView>("settings.get", {})).settings.find((s) => s.id === HIDDEN)?.value;

test.beforeAll(() => mkdirSync(SHOTS, { recursive: true }));
test.afterEach(async () => {
  await control("settings.set", { id: HIDDEN });
});

test("dk1 an app hidden from search is listed in Settings and shown again from there", async ({ page }) => {
  // The desktop hides two, as its Hide button does: Mail, and an app since removed.
  await control("settings.set", { id: HIDDEN, value: ["mail", "gone-app"] });
  expect(audit().some((r) => r.event.kind === "setting_chosen" && JSON.stringify(r.event.detail).includes("gone-app"))).toBe(true);

  await page.goto(appUrl("settings"));
  await expect(page.locator("html[data-ready=true]")).toHaveCount(1);
  const row = page.locator(`[data-setting="${HIDDEN}"]`);
  await expect(row.locator(".hidden-app")).toHaveText(["MailShow in search", "gone-appShow in search"]);
  await row.scrollIntoViewIfNeeded();
  await page.screenshot({ path: join(SHOTS, "settings-hidden-from-search.png") });

  await row.locator('[data-app="mail"] button').click();
  await expect(row.locator(".hidden-app")).toHaveCount(1);
  await expect(page.locator(".banner")).toHaveText("Hidden from search: gone-app.");
  expect(await hidden()).toEqual(["gone-app"]);

  await row.locator('[data-app="gone-app"] button').click();
  await expect(row).toContainText("None");
  await expect(page.locator(".banner")).toHaveText("Every app shows in search.");
  expect(await hidden()).toEqual([]);
});
