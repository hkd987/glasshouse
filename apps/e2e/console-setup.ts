// Brings up the console suite's system: the admin console in development
// mode with its fixture (500 devices, 50 catalog items), then a
// development-host glassd enrolled with it as device `dev-host`
// (tools/dev/host-run.sh with GLASS_DEV_ENROLL). Waits for the device's
// first sync to put the org's policy in force. Returns the teardown.

import { type ChildProcess, spawn } from "node:child_process";
import { existsSync, mkdirSync, openSync, readFileSync, rmSync } from "node:fs";
import { userInfo } from "node:os";
import { join } from "node:path";

import { BILLING_PORT, BILLING_STATE, CONSOLE_PORT, CONSOLE_STATE, DEVICE, FIXTURE, RELAY_API, STORE_PORT } from "./console-env.ts";
import { BIN, DEV, REPO, RUN, glassctlJson, sleep } from "./env.ts";

const LOGS = join(DEV, "..");

async function until(what: string, check: () => boolean, ms = 30_000): Promise<void> {
  const deadline = Date.now() + ms;
  while (Date.now() < deadline) {
    try {
      if (check()) return;
    } catch {
      // Not up yet.
    }
    await sleep(50);
  }
  throw new Error(`timed out waiting for ${what} (see ${LOGS}/*.log)`);
}

function start(name: string, command: string, args: string[], env: NodeJS.ProcessEnv = {}): ChildProcess {
  const log = openSync(join(LOGS, `${name}.log`), "w");
  return spawn(command, args, { cwd: REPO, env: { ...process.env, ...env }, stdio: ["ignore", log, log] });
}

async function stop(child: ChildProcess): Promise<void> {
  if (child.exitCode !== null || child.signalCode !== null) return;
  const exited = new Promise((resolve) => child.once("exit", resolve));
  child.kill("SIGTERM");
  await Promise.race([exited, sleep(10_000)]);
  if (child.exitCode === null && child.signalCode === null) child.kill("SIGKILL");
}

export default async function consoleSetup(): Promise<() => Promise<void>> {
  rmSync(LOGS, { recursive: true, force: true });
  rmSync(RUN, { recursive: true, force: true });
  mkdirSync(DEV, { recursive: true });
  if (!existsSync(join(REPO, "apps/console/dist/app.js"))) {
    throw new Error("the console's web app isn't built: make apps-build");
  }
  const started: ChildProcess[] = [];
  try {
    // The meeting relay, which the console admits people to (ADR 0030 §6).
    started.push(start("relay", join(BIN, "glass-sfu"), ["--udp", "127.0.0.1:0", "--api", RELAY_API]));
    await until("the relay", () => readFileSync(join(LOGS, "relay.log"), "utf8").includes("glass-sfu ready"));
    // The billing relay: Stripe's webhooks reach it, and it pushes each
    // payment to the console, signed with its key (ADR 0040).
    rmSync(BILLING_STATE, { recursive: true, force: true });
    started.push(
      start("billing", join(BIN, "glass-billing"), [
        "--dev",
        "--listen",
        `127.0.0.1:${BILLING_PORT}`,
        "--state",
        BILLING_STATE,
        "--console",
        `acme=http://127.0.0.1:${CONSOLE_PORT}`,
        "--first-retry-secs",
        "1",
      ]),
    );
    let billingKey = "";
    await until("the billing relay", () => {
      billingKey = /glass-billing ready http=\S+ key=([0-9a-f]+)/.exec(readFileSync(join(LOGS, "billing.log"), "utf8"))?.[1] ?? "";
      return billingKey !== "";
    });
    started.push(
      start("console", join(BIN, "glass-console"), [
        "--billing-relay",
        `http://127.0.0.1:${BILLING_PORT}`,
        "--billing-key",
        billingKey,
        "--relay",
        RELAY_API,
        "--dev",
        "--listen",
        `127.0.0.1:${CONSOLE_PORT}`,
        "--state",
        CONSOLE_STATE,
        "--web",
        join(REPO, "apps/console/dist"),
        "--fixture",
        FIXTURE.join(","),
        "--admin",
        userInfo().username,
      ]),
    );
    await until("the console", () => existsSync(join(CONSOLE_STATE, "enrollment.json")));
    started.push(
      start("glassd", join(REPO, "tools/dev/host-run.sh"), [], {
        GLASS_DEV_DIR: DEV,
        GLASS_DEV_RUN: RUN,
        GLASS_DEV_SKIP_BUILD: "1",
        GLASS_DEV_ENROLL: join(CONSOLE_STATE, "enrollment.json"),
        GLASS_DEV_DEVICE: DEVICE,
      }),
    );
    await until("glassd", () => existsSync(join(RUN, "control.sock")));
    await until("the org's policy on the device", () =>
      glassctlJson<{ policy: string }>("status").policy.startsWith("managed v"),
    );
    // The device's apps, for the Store (DV-11).
    started.push(
      start("bridge", join(BIN, "glass-app-bridge"), [
        "--dev",
        "--listen",
        `127.0.0.1:${STORE_PORT}`,
        "--apps",
        join(REPO, "apps"),
        "--socket",
        join(RUN, "apps.sock"),
      ]),
    );
  } catch (error) {
    for (const child of started.reverse()) await stop(child);
    throw error;
  }
  return async () => {
    for (const child of started.reverse()) await stop(child);
    rmSync(RUN, { recursive: true, force: true });
  };
}
