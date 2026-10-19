// The apps' end-to-end suite (UI-5, UI-7): headless Chromium against the
// development host's glassd and the app bridge, one browser at a time.

import { defineConfig } from "@playwright/test";

// The machine's browsers; never `playwright install`.
process.env.PLAYWRIGHT_BROWSERS_PATH ??= "/opt/pw-browsers";

export default defineConfig({
  testDir: ".",
  testMatch: "*.spec.ts",
  globalSetup: "./global-setup.ts",
  outputDir: "results/artifacts",
  workers: 1,
  fullyParallel: false,
  retries: 0,
  timeout: 60_000,
  reporter: [["list"]],
  use: {
    browserName: "chromium",
    headless: true,
    viewport: { width: 1280, height: 800 },
    deviceScaleFactor: 1,
    locale: "en-US",
    launchOptions: {
      // Loopback only: no proxy between the browser and the bridge.
      args: ["--no-proxy-server"],
      ...(process.env.PW_CHROMIUM ? { executablePath: process.env.PW_CHROMIUM } : {}),
    },
  },
});
