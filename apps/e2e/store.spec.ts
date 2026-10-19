// The Store on a computer no organization manages (DV-13): it says there's
// no catalog to show rather than looking broken. The console suite drives
// it on a managed device (console.e2e.ts).

import { mkdirSync } from "node:fs";
import { join } from "node:path";
import { expect, test } from "@playwright/test";

import { SHOTS, appUrl } from "./env.ts";

test.beforeAll(() => mkdirSync(SHOTS, { recursive: true }));

test("dv13 an unmanaged computer's Store says there's no catalog, and why", async ({ page }) => {
  await page.goto(appUrl("store"));
  await expect(page.locator("html[data-ready=true]")).toHaveCount(1);
  await expect(page.locator(".empty")).toContainText("isn't managed by an organization");
  await page.screenshot({ path: join(SHOTS, "store-unmanaged.png") });
});
