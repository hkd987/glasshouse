// Settings on the computer (UX-1): what the person hid from Ctrl Space
// (DK-1) is listed under Search, by the names the computer offers the apps
// under (an id for one it no longer has), and comes back one at a time.
// The suite's computer is the suite user's own (ADR 0038): they change its
// own settings too, for everyone on it.

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
  await control("settings.set", { id: USB });
  await control("settings.set", { id: HOME_PAGE });
});

const USB = "devices.usb_storage";
const HOME_PAGE = "browser.home_page";
const value = async (id: string) => (await control<SettingsView>("settings.get", {})).settings.find((s) => s.id === id)?.value;

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

test("adr0038 the owner of a computer no organization manages turns its own settings on and off", async ({ page }) => {
  await page.goto(appUrl("settings"));
  await expect(page.locator("html[data-ready=true]")).toHaveCount(1);
  const usb = page.locator(`[data-setting="${USB}"]`);
  await expect(usb).toContainText("This computer's default; you choose for everyone on it");
  await usb.locator("input.switch").uncheck();
  await expect(page.locator(".banner")).toHaveText("USB drives is off.");
  await expect(usb).toContainText("Your choice, for everyone on this computer");
  expect(await value(USB)).toBe(false);

  const home = page.locator(`[data-setting="${HOME_PAGE}"]`);
  await home.locator("input.text").fill("https://intranet.example");
  await home.locator("input.text").press("Enter");
  await expect(page.locator(".banner")).toHaveText("Home page is https://intranet.example.");
  expect(await value(HOME_PAGE)).toBe("https://intranet.example");
  await usb.scrollIntoViewIfNeeded();
  await page.screenshot({ path: join(SHOTS, "settings-owner.png") });

  await usb.getByRole("button", { name: "Use default" }).click();
  await expect(usb.locator("input.switch")).toBeChecked();
});
