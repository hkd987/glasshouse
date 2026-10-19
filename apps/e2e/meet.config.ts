// Meetings' media path (CF-21, ADR 0030 §6, slice M1): headless Chromium
// with its fake camera and microphone, the Meet page and the relay
// (glass-sfu). The test plays glassd and the console: it answers the
// page's bridge calls and hands each offer to the relay's private API, so
// it can also seat someone holding the wrong key. `make meet-test` runs it.

import { defineConfig } from "@playwright/test";

// The machine's browsers; never `playwright install`.
process.env.PLAYWRIGHT_BROWSERS_PATH ??= "/opt/pw-browsers";

export default defineConfig({
  testDir: ".",
  testMatch: ["meet.e2e.ts"],
  outputDir: "results/meet-artifacts",
  workers: 1,
  fullyParallel: false,
  retries: 0,
  timeout: 60_000,
  reporter: [["list"]],
  use: {
    browserName: "chromium",
    headless: true,
    viewport: { width: 960, height: 600 },
    deviceScaleFactor: 1,
    locale: "en-US",
    launchOptions: {
      args: [
        "--no-proxy-server",
        // The engine's own test camera and microphone, with no prompt.
        "--use-fake-device-for-media-stream",
        "--use-fake-ui-for-media-stream",
        // The relay listens on loopback here.
        "--allow-loopback-in-peer-connection",
      ],
      ...(process.env.PW_CHROMIUM ? { executablePath: process.env.PW_CHROMIUM } : {}),
    },
  },
});
