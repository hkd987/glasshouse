// Where the console suite's console lives (console.config.ts sets where
// its device lives, for env.ts).

import { readFileSync } from "node:fs";
import { join } from "node:path";

import { REPO } from "./env.ts";

export const CONSOLE_STATE = process.env.GLASS_CONSOLE_E2E_STATE ?? join(REPO, "build/console-e2e/console");
export const CONSOLE_PORT = Number(process.env.GLASS_CONSOLE_E2E_PORT ?? 7410);
export const CONSOLE_URL = `http://127.0.0.1:${CONSOLE_PORT}/`;
/** The device's app bridge, for the Store (DV-11). */
export const STORE_PORT = Number(process.env.GLASS_CONSOLE_E2E_STORE_PORT ?? 7411);
export const STORE_URL = `http://store.localhost:${STORE_PORT}/`;
/** Mail and Calendar on the same bridge, with the org's directory (AD-1). */
export const MAIL_URL = `http://mail.localhost:${STORE_PORT}/`;
export const CALENDAR_URL = `http://calendar.localhost:${STORE_PORT}/`;
/** Settings on the same bridge: what IT set shows there (UX-1). */
export const SETTINGS_URL = `http://settings.localhost:${STORE_PORT}/`;
/** Chat on the same bridge: the org's chat, sealed end to end (CF-91). */
export const CHAT_URL = `http://chat.localhost:${STORE_PORT}/`;
/** Artifacts on the same bridge, and each artifact's own origin beside it (ADR 0033). */
export const ARTIFACTS_URL = `http://artifacts.localhost:${STORE_PORT}/`;
/** Meet on the computer (CF-21). */
export const MEET_URL = `http://meet.localhost:${STORE_PORT}/`;
/** The meeting relay's private API, beside the console (ADR 0030 §6). */
export const RELAY_API = `127.0.0.1:${Number(process.env.GLASS_CONSOLE_E2E_RELAY_PORT ?? 7412)}`;
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
