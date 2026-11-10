// Brings up the system the apps run against, as `make dev-host` does, in
// the suite's own folder: glassd on the development host, mail and events
// people sent, an agent's work through the real pipeline (the user's own
// Claude Code drafts a reply and holds time, "by claude-code for <user>"),
// and the app bridge in dev mode. Returns the teardown, which stops both.

import { type ChildProcess, spawn } from "node:child_process";
import { existsSync, mkdirSync, openSync, rmSync, writeFileSync } from "node:fs";
import { createConnection } from "node:net";
import { userInfo } from "node:os";
import { join } from "node:path";

import { BIN, DEV, PORT, REPO, RUN, glassctl, glassctlJson, mcp, sleep, takesTasks } from "./env.ts";

async function until(what: string, check: () => boolean | Promise<boolean>, ms = 20_000): Promise<void> {
  const deadline = Date.now() + ms;
  while (Date.now() < deadline) {
    if (await check()) return;
    await sleep(50);
  }
  throw new Error(`timed out waiting for ${what} (see ${DEV}/*.log)`);
}

function start(name: string, command: string, args: string[], env: NodeJS.ProcessEnv = {}): ChildProcess {
  const log = openSync(join(DEV, `${name}.log`), "w");
  const child = spawn(command, args, {
    cwd: REPO,
    env: { ...process.env, ...env },
    stdio: ["ignore", log, log],
  });
  child.on("exit", (code, signal) => {
    if (code !== 0 && signal === null) console.error(`${name} exited with ${code}; see ${DEV}/${name}.log`);
  });
  return child;
}

async function stop(child: ChildProcess): Promise<void> {
  if (child.exitCode !== null || child.signalCode !== null) return;
  const exited = new Promise((resolve) => child.once("exit", resolve));
  child.kill("SIGTERM");
  await Promise.race([exited, sleep(10_000)]);
  if (child.exitCode === null && child.signalCode === null) child.kill("SIGKILL");
}

const pad = (n: number) => String(n).padStart(2, "0");
const wall = (d: Date) =>
  `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
/** This week's day `day` (0 = Monday) at `hh:mm`, local time. */
function weekday(day: number, hh: number, mm = 0): string {
  const now = new Date();
  const monday = new Date(now.getFullYear(), now.getMonth(), now.getDate() - ((now.getDay() + 6) % 7));
  return wall(new Date(monday.getFullYear(), monday.getMonth(), monday.getDate() + day, hh, mm));
}

/** Mail people sent the user: the mail service isn't there yet, so the inbox is what's filed. */
function seedInbox(home: string, user: string): void {
  const inbox = join(home, "Mail/Inbox");
  mkdirSync(inbox, { recursive: true });
  const ago = (minutes: number) => new Date(Date.now() - minutes * 60_000).toISOString().replace(/\.\d+Z$/, "Z");
  const messages = [
    ["Dana Ruiz <dana@northwind.example>", "Renewal terms for next year", ago(18),
      "Hi,\n\nBefore we sign the renewal, could we move the start to January 1 and keep this year's rate for the first quarter?\n\nHappy to talk it through Tuesday or Wednesday.\n\nThanks,\nDana"],
    ["Priya Shah <priya@corp.example>", "Wire memo for Thursday", ago(52),
      "Can you look over the wire memo before 2:30 tomorrow? The amounts match the October schedule; I only changed the reference line.\n\nPriya"],
    ["Northwind AP <ap@northwind.example>", "Invoice INV-2026-014 paid", ago(60 * 26),
      "Payment for INV-2026-014 ($18,400.00) was sent today by ACH. It should arrive in two business days."],
    ["IT Service Desk <it@corp.example>", "Laptop updates tonight", ago(60 * 30),
      "Your laptop will install updates tonight after 10 PM. Save your work and leave it plugged in. Nothing else is needed from you."],
    ["Marco Bianchi <marco@fabrikam.example>", "Q4 forecast review", ago(60 * 52),
      "Sharing the Q4 forecast ahead of Wednesday. The services line moved up 4%; the rest is as planned."],
    ["Team Offsite <offsite@corp.example>", "Offsite agenda draft", ago(60 * 24 * 4),
      "Here's the first draft of the offsite agenda. Comments by Friday, please."],
  ] as const;
  messages.forEach(([from, subject, date, body], i) => {
    const slug = subject.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "");
    const stamp = Math.floor(new Date(date).getTime() / 1000);
    writeFileSync(
      join(inbox, `${stamp}-${i}-${slug}.eml`),
      `From: ${from}\nTo: ${user}@corp.example\nSubject: ${subject}\nDate: ${date}\n\n${body}\n`,
    );
  });
}

/** The user's week: their own events and invitations others sent. */
function seedCalendar(home: string, user: string): void {
  const events = join(home, "Calendar/Events");
  mkdirSync(events, { recursive: true });
  const write = (id: string, title: string, start: string, end: string, extra: Record<string, unknown> = {}) =>
    writeFileSync(
      join(events, `${id}.json`),
      JSON.stringify({ id, title, start, end, kind: "event", location: "", notes: "", attendees: [], organizer: user, attribution: "", ...extra }, null, 2),
    );
  for (let day = 0; day < 5; day++) {
    write(`standup-${day}`, "Standup", weekday(day, 9, 0), weekday(day, 9, 15), { location: "Room 4B" });
  }
  write("one-on-one", "1:1 with Priya", weekday(0, 13), weekday(0, 13, 45));
  write("renewal-call", "Northwind renewal call", weekday(1, 10), weekday(1, 11), {
    organizer: "dana@northwind.example",
    attendees: ["dana@northwind.example", `${user}@corp.example`],
    location: "Video call",
  });
  write("forecast", "Q4 forecast review", weekday(2, 11), weekday(2, 12), {
    organizer: "marco@fabrikam.example",
    attendees: ["marco@fabrikam.example", `${user}@corp.example`],
  });
  write("design-lunch", "Lunch with design", weekday(2, 12, 30), weekday(2, 13, 30));
  write("offsite", "Offsite planning", weekday(3, 14), weekday(3, 15, 30), { location: "Room 2A" });
  write("wrap-up", "Week wrap-up", weekday(4, 16), weekday(4, 16, 30));
}

export default async function globalSetup(): Promise<() => Promise<void>> {
  rmSync(DEV, { recursive: true, force: true });
  rmSync(RUN, { recursive: true, force: true });
  mkdirSync(DEV, { recursive: true });
  const user = userInfo().username;
  const home = join(DEV, "home");
  seedInbox(home, user);
  seedCalendar(home, user);
  // The suite's user set up this computer, which no organization manages:
  // its owner (ADR 0038), as first-run setup records.
  mkdirSync(join(DEV, "state"), { recursive: true });
  writeFileSync(join(DEV, "state/setup.toml"), `owner = "${user}"\nmodels = "later"\n`);

  const glassd = start("glassd", join(REPO, "tools/dev/host-run.sh"), [], {
    GLASS_DEV_DIR: DEV,
    GLASS_DEV_RUN: RUN,
    GLASS_DEV_SKIP_BUILD: "1",
    // The desktop agent on the scripted model: Routines' tests have it do
    // a request twice the same way (ADR 0047).
    GLASS_DEV_SCRIPTED_MODEL: "1",
  });
  const started: ChildProcess[] = [glassd];
  try {
    await until("glassd", () => existsSync(join(RUN, "apps.sock")) && existsSync(join(RUN, "control.sock")));
    await until("glassd's check of its system files", takesTasks);
    started.push(await prepare());
  } catch (error) {
    for (const child of started.reverse()) await stop(child);
    throw error;
  }
  return async () => {
    for (const child of started.reverse()) await stop(child);
    rmSync(RUN, { recursive: true, force: true });
  };
}

/** The agent's work, then the bridge. Returns the bridge. */
async function prepare(): Promise<ChildProcess> {
  // The user's own Claude Code drafts a reply and holds time to prepare:
  // real calls through the task socket and the pipeline (AU-10).
  const task = glassctlJson<{ id: string; driver_socket: string }>(
    "run",
    "claude-code",
    "Triage my inbox: draft a reply to Dana and hold time to prepare for the renewal call",
    "--title",
    "Inbox triage",
    "--yes",
  );
  await until("the agent's task socket", () => existsSync(task.driver_socket));
  const results = await mcp(task.driver_socket, [
    {
      name: "mail_draft",
      arguments: {
        to: ["dana@northwind.example"],
        subject: "Re: Renewal terms for next year",
        body:
          "Hi Dana,\n\nThanks for the note. January 1 works for us, and keeping this year's rate through March is fine on our side.\n\nTuesday at 10 would be a good time to walk through the rest.\n\nBest regards",
      },
    },
    {
      name: "calendar_hold",
      arguments: {
        title: "Prep for Northwind renewal",
        start: weekday(0, 15),
        end: weekday(0, 16),
        notes: "Read Dana's note and last year's terms before Tuesday's call.",
      },
    },
  ]);
  for (const result of results) {
    if ((result as { isError?: boolean; error?: unknown }).isError || (result as { error?: unknown }).error) {
      throw new Error(`the agent's call failed: ${JSON.stringify(result)}`);
    }
  }
  glassctl("stop", task.id);

  const bridge = start("bridge", join(BIN, "glass-app-bridge"), [
    "--dev",
    "--listen",
    `127.0.0.1:${PORT}`,
    "--apps",
    join(REPO, "apps"),
    "--socket",
    join(RUN, "apps.sock"),
  ]);
  await until(
    "the app bridge",
    () =>
      new Promise<boolean>((resolve) => {
        const probe = createConnection({ host: "127.0.0.1", port: PORT });
        probe.once("connect", () => {
          probe.destroy();
          resolve(true);
        });
        probe.once("error", () => resolve(false));
      }),
  );

  return bridge;
}
