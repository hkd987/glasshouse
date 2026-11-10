// The admin console's end-to-end suite (ADR 0023): headless Chromium
// driving the console (glass-console --dev, with a 500-device, 50-item
// fixture) and a development-host glassd enrolled with it, one browser at
// a time. `make console-test` runs it.

import { userInfo, tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { defineConfig } from "@playwright/test";

import { suiteFolder } from "./folders.ts";

const repo = join(dirname(fileURLToPath(import.meta.url)), "../..");
// The machine's browsers; never `playwright install`.
process.env.PLAYWRIGHT_BROWSERS_PATH ??= "/opt/pw-browsers";
// The device's state and sockets, where env.ts (glassctl, the audit log)
// finds them. Set before any test module loads env.ts.
process.env.GLASS_APPS_E2E_DIR ??= suiteFolder(join(repo, "build/console-e2e/device"), "console-device");
process.env.GLASS_APPS_E2E_RUN ??= join(tmpdir(), `glass-console-e2e-${userInfo().uid}`);

export default defineConfig({
  testDir: ".",
  // Chat's and Artifacts' tests need only what the setup brings up: the
  // org's first policy and its Chat release. Sharing an app (studio.e2e.ts)
  // needs the console's confined bundler too, which it finds in
  // apps/node_modules.
  testMatch: ["console.e2e.ts", "chat.e2e.ts", "artifacts.e2e.ts", "docs.e2e.ts", "meetings.e2e.ts", "studio.e2e.ts"],
  globalSetup: "./console-setup.ts",
  outputDir: "results/console-artifacts",
  workers: 1,
  fullyParallel: false,
  retries: 0,
  timeout: 90_000,
  reporter: [["list"]],
  use: {
    browserName: "chromium",
    headless: true,
    viewport: { width: 1440, height: 900 },
    deviceScaleFactor: 1,
    locale: "en-US",
    launchOptions: {
      args: [
        "--no-proxy-server",
        // Meetings (CF-21): the engine's test camera and microphone, with
        // no prompt, and the relay on loopback.
        "--use-fake-device-for-media-stream",
        "--use-fake-ui-for-media-stream",
        "--allow-loopback-in-peer-connection",
      ],
      ...(process.env.PW_CHROMIUM ? { executablePath: process.env.PW_CHROMIUM } : {}),
    },
  },
});
