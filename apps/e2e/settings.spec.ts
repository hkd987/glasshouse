// Settings on the computer (UX-1): what the person hid from Ctrl Space
// (DK-1) is listed under Search, by the names the computer offers the apps
// under (an id for one it no longer has), and comes back one at a time.
// The suite's computer is the suite user's own (ADR 0038): they change its
// own settings too, for everyone on it.

import { existsSync, mkdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { expect, test } from "@playwright/test";

import { DEV, SHOTS, appUrl, audit, control, glassctlJson } from "./env.ts";

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

/** A network's row in Settings' Wi-Fi section. */
const wifiRow = (page: import("@playwright/test").Page, name: string) => page.locator(`[data-section=wifi] [data-network="${name}"]`);

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

test("adr0039 the owner connects their own AI models in Settings, and the key stays with the system", async ({ page }) => {
  const key = "sk-or-v1-e2e-own-key-not-real";
  await page.goto(appUrl("settings"));
  await expect(page.locator("html[data-ready=true]")).toHaveCount(1);
  const row = page.locator('[data-setting="models"]');
  await row.scrollIntoViewIfNeeded();
  await row.locator("select[name=service]").selectOption("openrouter");
  await row.locator("input[name=key]").fill(key);
  await row.locator("[data-test=connect]").click();
  await expect(page.locator(".banner")).toContainText("your OpenRouter account");
  await expect(row).toContainText("Your OpenRouter account");
  await page.screenshot({ path: join(SHOTS, "settings-ai-models.png") });

  // Kept where only the system reads it; never on the record or in a log.
  const file = join(DEV, "state/models/personal.toml");
  expect(statSync(file).mode & 0o077).toBe(0);
  expect(JSON.stringify(audit())).not.toContain("e2e-own-key");
  expect(audit().some((r) => r.event.kind === "action" && r.event.action === "settings.models_connect")).toBe(true);
  for (const log of ["glassd.log", "bridge.log"]) {
    const path = join(DEV, log);
    if (existsSync(path)) expect(readFileSync(path, "utf8")).not.toContain("e2e-own-key");
  }

  // The IT agent may use them until the owner says otherwise: its card
  // names the model. Turned off, it's scripted again; the choice is on the
  // record (ADR 0039).
  const model = () => glassctlJson<{ model: string }>("plan", "it-agent", "My disk is full").model;
  const it = page.locator('[data-setting="models-agent-it-agent"]');
  await expect(it.locator("input[type=checkbox]")).toBeChecked();
  await expect(page.locator('[data-setting="models-agent-skill-evals"] input')).toBeDisabled();
  expect(model()).not.toBe("No model (scripted agent)");
  await page.locator('[data-setting="models-agent-skill-evals"]').scrollIntoViewIfNeeded();
  await page.screenshot({ path: join(SHOTS, "settings-ai-models-agents.png") });
  await it.locator("input[type=checkbox]").uncheck();
  await expect(page.locator(".banner")).toContainText("IT agent no longer uses your AI models.");
  await expect(it.locator("input[type=checkbox]")).not.toBeChecked();
  expect(model()).toBe("No model (scripted agent)");
  expect(audit().some((r) => r.event.kind === "action" && r.event.action === "settings.models_agents")).toBe(true);
  await it.locator("input[type=checkbox]").check();
  await expect(page.locator(".banner")).toContainText("IT agent may use your AI models now.");
  expect(model()).not.toBe("No model (scripted agent)");

  await row.locator("[data-test=disconnect]").click();
  await expect(row.locator("[data-test=connect]")).toBeVisible();
  expect(existsSync(file)).toBe(false);
  expect(model()).toBe("No model (scripted agent)");
});

test("cf12 the person joins Wi-Fi in Settings, inside the rules the computer's owner sets", async ({ page }) => {
  // The suite's Wi-Fi is glass-fake-wpa: wpa_supplicant's control socket
  // with networks in the air (tools/dev/host-run.sh).
  await page.goto(appUrl("settings"));
  await expect(page.locator("html[data-ready=true]")).toHaveCount(1);
  const section = page.locator("[data-section=wifi]");
  await expect(section.locator("[data-network]")).toHaveCount(5);
  await expect(wifiRow(page, "Northwind Corp")).toContainText("needs a sign-in");
  await expect(wifiRow(page, "Northwind Corp").locator("[data-test=wifi-join]")).toHaveCount(0);

  // An open network joins at once.
  await wifiRow(page, "Café Guest").locator("[data-test=wifi-join]").click();
  await expect(page.locator(".banner")).toHaveText("Joining Café Guest.");
  await expect(wifiRow(page, "Café Guest")).toContainText("Connected · 192.168.4.20");

  // One with a password asks for it: a wrong one doesn't connect, the right one does.
  const home = wifiRow(page, "Home 6E");
  await home.locator("[data-test=wifi-join]").click();
  await home.locator("input[name=password]").fill("not the password");
  await home.locator("[data-test=wifi-join]").click();
  await expect(page.locator(".banner")).toHaveText("Joining Home 6E.");
  await expect(home).toContainText("Couldn't join: the password may be wrong");
  await home.locator("[data-test=wifi-join]").click();
  await home.locator("input[name=password]").fill("correct horse battery");
  await home.locator("[data-test=wifi-join]").click();
  await expect(home).toContainText("Connected · 192.168.4.20");
  await section.scrollIntoViewIfNeeded();
  await page.screenshot({ path: join(SHOTS, "settings-wifi.png") });

  // The owner blocks a network and names the computer's own: the blocked one
  // can't be joined, the computer's joins by itself, its password from the
  // system's file, and can't be forgotten here.
  const blocked = page.locator('[data-setting="wifi.blocked"] textarea');
  await blocked.fill("Free Airport WiFi");
  await blocked.blur();
  await expect(wifiRow(page, "Free Airport WiFi")).toContainText("Free Airport WiFi is blocked on this computer.");
  await expect(wifiRow(page, "Free Airport WiFi").locator("[data-test=wifi-join]")).toHaveCount(0);
  const ours = page.locator('[data-setting="wifi.networks"] textarea');
  await ours.fill("Northwind Staff");
  await ours.blur();
  await expect(wifiRow(page, "Northwind Staff")).toContainText("Your organization's network");
  await expect(wifiRow(page, "Northwind Staff").locator("[data-test=wifi-forget]")).toHaveCount(0);

  // Forgetting Home 6E: the computer falls back on its own network by itself.
  await home.locator("[data-test=wifi-forget]").click();
  await expect(page.locator(".banner")).toHaveText("Forgot Home 6E.");
  await expect(wifiRow(page, "Northwind Staff")).toContainText("Connected · 192.168.4.20");
  await section.scrollIntoViewIfNeeded();
  await page.screenshot({ path: join(SHOTS, "settings-wifi-rules.png") });

  // On the record; never the password, in the record or a log.
  expect(audit().some((r) => r.event.kind === "action" && r.event.action === "settings.wifi_join")).toBe(true);
  expect(JSON.stringify(audit())).not.toContain("correct horse");
  expect(JSON.stringify(audit())).not.toContain("staff-password");
  for (const log of ["glassd.log", "bridge.log"]) {
    const path = join(DEV, log);
    if (existsSync(path)) expect(readFileSync(path, "utf8")).not.toContain("correct horse");
  }

  // Back as it was.
  await blocked.fill("");
  await blocked.blur();
  await ours.fill("");
  await ours.blur();
  await expect(wifiRow(page, "Northwind Staff")).not.toContainText("Your organization's network");
  for (const name of ["Café Guest"]) {
    if (await wifiRow(page, name).locator("[data-test=wifi-forget]").count()) await wifiRow(page, name).locator("[data-test=wifi-forget]").click();
  }
});
