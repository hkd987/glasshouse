// Where the console suite's console lives (console.config.ts sets where
// its device lives, for env.ts).

import { readFileSync } from "node:fs";
import { join } from "node:path";

import { REPO } from "./env.ts";

export const CONSOLE_STATE = process.env.GLASS_CONSOLE_E2E_STATE ?? join(REPO, "build/console-e2e/console");
export const CONSOLE_PORT = Number(process.env.GLASS_CONSOLE_E2E_PORT ?? 7410);
export const CONSOLE_URL = `http://127.0.0.1:${CONSOLE_PORT}/`;
/** The fixture: devices, catalog items. */
export const FIXTURE = [500, 50] as const;
/** This device's id in the fleet. */
export const DEVICE = "dev-host";
/** Screenshots, 1440x900 (`SHOTS_DIR=...` to keep them elsewhere). */
export const CONSOLE_SHOTS = process.env.SHOTS_DIR ?? join(REPO, "apps/e2e/results/console-shots");

export interface Enrollment {
  url: string;
  org: string;
  org_key: string;
  token: string;
}

export function enrollment(): Enrollment {
  return JSON.parse(readFileSync(join(CONSOLE_STATE, "enrollment.json"), "utf8")) as Enrollment;
}
