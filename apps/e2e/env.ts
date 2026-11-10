// Where the e2e suite's system lives, and how tests reach it from outside
// the apps: glassctl on the control socket (the desktop's side, which
// answers asks) and the audit log.

import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { createConnection } from "node:net";
import { tmpdir, userInfo } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { suiteFolder } from "./folders.ts";

export const REPO = join(dirname(fileURLToPath(import.meta.url)), "../..");
export const BIN = join(process.env.CARGO_TARGET_DIR ?? join(REPO, "target"), "debug");
/** The suite's own dev-host state (never the developer's build/dev); see folders.ts. */
export const DEV = process.env.GLASS_APPS_E2E_DIR ?? suiteFolder(join(REPO, "build/apps-e2e"), "apps");
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
    detail?: unknown;
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

/** One MCP session on a task socket, as a harness speaks it (ADR 0004). */
export async function mcp(socket: string, calls: { name: string; arguments: unknown }[]): Promise<unknown[]> {
  const conn = createConnection(socket);
  await new Promise<void>((resolve, reject) => conn.once("connect", resolve).once("error", reject));
  let buffer = "";
  const waiting = new Map<number, (value: unknown) => void>();
  conn.on("data", (chunk) => {
    buffer += chunk.toString();
    let newline: number;
    while ((newline = buffer.indexOf("\n")) >= 0) {
      const line = buffer.slice(0, newline);
      buffer = buffer.slice(newline + 1);
      if (!line.trim()) continue;
      const message = JSON.parse(line) as { id?: number; result?: unknown; error?: unknown };
      if (message.id !== undefined) waiting.get(message.id)?.(message.error ? { error: message.error } : message.result);
    }
  });
  let id = 0;
  const request = (method: string, params: unknown) =>
    new Promise<unknown>((resolve) => {
      id += 1;
      waiting.set(id, resolve);
      conn.write(`${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`);
    });
  await request("initialize", {
    protocolVersion: "2025-06-18",
    capabilities: {},
    clientInfo: { name: "apps-e2e", version: "0" },
  });
  conn.write(`${JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" })}\n`);
  const results = [];
  for (const call of calls) results.push(await request("tools/call", call));
  conn.end();
  return results;
}

/** One call on the control socket, as the desktop makes it. */
export async function control<T>(method: string, params: unknown): Promise<T> {
  const conn = createConnection(join(RUN, "control.sock"));
  await new Promise<void>((resolve, reject) => conn.once("connect", resolve).once("error", reject));
  let buffer = "";
  const reply = new Promise<T>((resolve, reject) =>
    conn.on("data", (chunk) => {
      buffer += chunk.toString();
      const newline = buffer.indexOf("\n");
      if (newline < 0) return;
      const message = JSON.parse(buffer.slice(0, newline)) as { result?: T; error?: { message: string } };
      if (message.error) reject(new Error(message.error.message));
      else resolve(message.result as T);
    }),
  );
  conn.write(`${JSON.stringify({ jsonrpc: "2.0", id: 1, method, params })}\n`);
  try {
    return await reply;
  } finally {
    conn.end();
  }
}
