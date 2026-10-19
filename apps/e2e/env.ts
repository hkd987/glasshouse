// Where the e2e suite's system lives, and how tests reach it from outside
// the apps: glassctl on the control socket (the desktop's side, which
// answers asks) and the audit log.

import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { tmpdir, userInfo } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

export const REPO = join(dirname(fileURLToPath(import.meta.url)), "../..");
export const BIN = join(process.env.CARGO_TARGET_DIR ?? join(REPO, "target"), "debug");
/** The suite's own dev-host state (never the developer's build/dev). */
export const DEV = process.env.GLASS_APPS_E2E_DIR ?? join(REPO, "build/apps-e2e");
/** Its sockets: short paths, since a Unix socket's must stay under 104 bytes. */
export const RUN = process.env.GLASS_APPS_E2E_RUN ?? join(tmpdir(), `glass-apps-e2e-${userInfo().uid}`);
export const PORT = Number(process.env.GLASS_APPS_E2E_PORT ?? 7310);
/** Where screenshots go (`SHOTS_DIR=...` to keep them elsewhere). */
export const SHOTS = process.env.SHOTS_DIR ?? join(REPO, "apps/e2e/results/shots");

export const appUrl = (app: string) => `http://${app}.localhost:${PORT}/`;

/** Runs glassctl against the suite's glassd, as the desktop would. */
export function glassctl(...args: string[]): string {
  return execFileSync(join(BIN, "glassctl"), args, {
    env: { ...process.env, GLASS_CONTROL_SOCKET: join(RUN, "control.sock") },
    encoding: "utf8",
  });
}

export function glassctlJson<T>(...args: string[]): T {
  return JSON.parse(glassctl("--json", ...args)) as T;
}

export interface ApprovalView {
  id: string;
  task: string;
  agent: string;
  summary: string;
  items: string[];
}

export interface AuditRecord {
  event: {
    kind: string;
    agent?: string;
    user?: string;
    action?: string;
    outcome?: { status: string };
    decision?: { tier: string; rule: string };
    summary: string;
  };
}

/** The audit log's records, oldest first. */
export function audit(): AuditRecord[] {
  return readFileSync(join(DEV, "state/audit/audit.log"), "utf8")
    .split("\n")
    .filter(Boolean)
    .map((line) => (JSON.parse(line) as { body: AuditRecord }).body);
}

/** "completed", "blocked", "denied", ... */
export function outcome(record: AuditRecord): string {
  return record.event.outcome?.status ?? "";
}

export const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));
