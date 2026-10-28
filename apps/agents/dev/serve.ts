// A stand-in for the front (glass-agents), for working on the web app
// before it's there: serves apps/agents/dist and answers the API from
// test/fixture.ts, with agents that change when you approve, ask, take
// over and type. Node's own http, no dependencies; state is in memory and
// starts over each run. Development only.
//
//   cd apps && node build.mjs && node agents/dev/serve.ts   # http://127.0.0.1:7510
//
// PORT sets the port (7510; the real front's is 7500); AGENTS_DEV_SPEED=0.2
// makes agents answer 5 times faster. /dev/sign-out signs the page out, to
// see that screen; /dev/sign-in signs it back in.

import { readFile } from "node:fs/promises";
import { type IncomingMessage, type ServerResponse, createServer } from "node:http";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import type { AgentDetail, AgentView, Answer, ApprovalView, Ask, Input, Message, NewAgent, Screen, SignIn, UndoResult } from "../src/api.ts";
import { type World, fixture, pendingOf } from "../test/fixture.ts";

const here = dirname(fileURLToPath(import.meta.url));
const DIST = join(here, "../dist");
const PORT = Number(process.env.PORT ?? 7510);
const SPEED = Number(process.env.AGENTS_DEV_SPEED ?? 1);
/** The policy the front serves the page with: its own files and API only. */
const CSP = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";
const LONG_POLL_MS = 25_000;

const world: World = fixture();
const screens = new Map<string, Buffer>(Object.entries(world.screens).map(([id, text]) => [id, Buffer.from(text)]));
const before = new Map<string, { status: AgentView["status"]; doing: string }>();
let cursor = 0;
const changes: { cursor: number; agent: string }[] = [];
const waiters = new Set<() => void>();
let next = 1000;
let signedOut = false;

// --- The world changing ----------------------------------------------------

const now = () => new Date().toISOString().replace(/\.\d{3}Z$/, "Z");
const later = (ms: number, fn: () => void) => setTimeout(fn, ms * SPEED);

function find(id: string): AgentDetail | undefined {
  return world.agents.find((d) => d.agent.id === id);
}

/** Something about `id` changed: wake the long polls. */
function bump(d: AgentDetail): void {
  d.agent.updated_at = now();
  d.agent.pending = d.messages.filter((m) => m.body.kind === "approval" && m.body.state.state === "pending").length;
  cursor += 1;
  changes.push({ cursor, agent: d.agent.id });
  for (const wake of waiters) wake();
  waiters.clear();
}

function say(d: AgentDetail, from: Message["from"], task: string, body: Message["body"]): Message {
  const m: Message = { id: `m${next++}`, at: now(), from, task, body };
  d.messages.push(m);
  return m;
}

function record(d: AgentDetail, text: string): void {
  d.record.push({ at: now(), text });
  if (d.record.length > 20) d.record.shift();
}

function print(id: string, text: string): void {
  screens.set(id, Buffer.concat([screens.get(id) ?? Buffer.alloc(0), Buffer.from(text.replace(/\n/g, "\r\n"))]));
}

/** Idle, or Needs you while a question waits. */
function settle(d: AgentDetail, doing: string): void {
  const waiting = d.messages.some((m) => m.body.kind === "approval" && m.body.state.state === "pending");
  d.agent.status = waiting ? "needs_you" : "idle";
  d.agent.doing = doing;
}

function prompt(id: string, who: string): string {
  return `\x1b[${who === "agent" ? "32" : "33"}m${who}@${id}\x1b[0m:\x1b[34m~\x1b[0m$ `;
}

/** What the person typed, run "in its computer". */
function run(d: AgentDetail, line: string): string {
  const [cmd = "", ...args] = line.trim().split(/\s+/);
  const id = d.agent.id;
  switch (cmd) {
    case "":
      return "";
    case "ls":
      return id === "reconciler" ? "in  match.py  out  draft_reminders.py\n" : "in  notes  out\n";
    case "pwd":
      return "/home/agent\n";
    case "whoami":
      return "dana\n";
    case "date":
      return `${new Date().toUTCString()}\n`;
    case "echo":
      return `${args.join(" ")}\n`;
    case "uname":
      return args.includes("-a") ? `Linux ${id} 6.12.8 #1 SMP PREEMPT_DYNAMIC x86_64 GNU/Linux\n` : "Linux\n";
    case "df":
      return `Filesystem      Size  Used Avail Use% Mounted on\n/dev/vdb         20G  ${(d.agent.computer.disk_bytes ?? 0) > 2e9 ? "2.4G" : "1.1G"}   18G  12% /home\n`;
    case "cat":
      return args.length ? args.map((f) => (/\.(xlsx|docx|pdf)$/.test(f) ? `cat: ${f}: a binary file, not shown\n` : `cat: ${f}: No such file or directory\n`)).join("") : "";
    case "help":
      return "Try: ls, pwd, whoami, date, uname -a, df -h, echo, cat\n";
    default:
      return `sh: ${cmd}: not found\n`;
  }
}

/** How a question turns out, per agent, once answered. */
function answered(d: AgentDetail, a: ApprovalView, yes: boolean, task: string): void {
  const id = d.agent.id;
  d.agent.status = "working";
  d.agent.doing = yes ? `${a.yes_label}…` : "Stopping, as you asked";
  record(d, `You ${yes ? "approved" : "said no to"}: ${a.confidential ? "a confidential question" : a.yes_label}`);
  print(id, yes ? `Approved by Dana. ${a.yes_label}…\n` : "Dana said no. Not sending.\n");
  bump(d);
  later(yes ? 1600 : 900, () => {
    if (yes) {
      const text = id === "reconciler" ? "Sent 3 emails. They’re in your Sent folder." : id === "contract-reviewer" ? "Sent the redline to Priya Shah at Baker & Lowe LLP. It’s in your Sent folder." : `Done: ${a.yes_label}.`;
      say(d, "agent", task, { kind: "done", text, undoable: false });
      record(d, `${a.yes_label} (${a.action})`);
      print(id, `Sent.\n${prompt(id, "agent")}`);
      settle(d, id === "reconciler" ? "Finished: sent 3 emails about March invoices" : `Finished: ${a.yes_label.toLowerCase()}`);
    } else {
      const text =
        id === "reconciler"
          ? "OK, I won’t send them. The sheet is in Finance/March if you’d like to send them yourself."
          : id === "contract-reviewer"
            ? "OK, I won’t send it. The redline stays in Legal/Northwind."
            : "OK, I won’t.";
      say(d, "agent", task, { kind: "text", text });
      print(id, prompt(id, "agent"));
      settle(d, "Stopped: you said no");
    }
    bump(d);
  });
}

function wake(d: AgentDetail, then?: () => void): void {
  d.agent.computer.state = "waking";
  d.agent.status = "starting";
  d.agent.doing = "Waking from storage";
  bump(d);
  later(1500, () => {
    d.agent.computer.state = "running";
    delete d.agent.computer.stored;
    say(d, "agent", d.messages[d.messages.length - 1]?.task ?? "t", { kind: "note", text: "Woken from storage: I picked up where I left off." });
    record(d, "Woke from storage");
    print(d.agent.id, `\n--- woken from storage ---\n${prompt(d.agent.id, "agent")}`);
    settle(d, "Awake and ready");
    bump(d);
    then?.();
  });
}

// --- The API ---------------------------------------------------------------

type Reply = { status: number; body: unknown };
const ok = (body: unknown, status = 200): Reply => ({ status, body });
const no = (status: number, error: string): Reply => ({ status, body: { error } });

async function api(method: string, path: string, query: URLSearchParams, body: unknown, req: IncomingMessage): Promise<Reply> {
  if (path === "/session" && method === "POST") {
    const who = (body as SignIn).person;
    if (who !== world.session.person.id) return no(403, `There’s no one called “${who}” here. Try ${world.session.person.id}.`);
    signedOut = false;
    return ok(world.session);
  }
  if (signedOut) return no(401, `Sign in: open http://127.0.0.1:${PORT}/dev/sign-in`);
  if (path === "/session" && method === "GET") return ok(world.session);
  if (path === "/agents" && method === "GET") return ok(world.agents.map((d) => d.agent));
  if (path === "/approvals" && method === "GET") return ok(pendingOf(world));
  if (path === "/templates" && method === "GET") return ok(world.templates);
  if (path === "/events" && method === "GET") return events(Number(query.get("since") ?? 0), req);
  if (path === "/agents" && method === "POST") return add(body as NewAgent);

  const answer = /^\/approvals\/([^/]+)$/.exec(path);
  if (answer && method === "POST") {
    const id = decodeURIComponent(answer[1] ?? "");
    for (const d of world.agents) {
      const m = d.messages.find((x) => x.body.kind === "approval" && x.body.id === id);
      if (!m || m.body.kind !== "approval") continue;
      if (m.body.state.state !== "pending") return no(409, "This was already answered.");
      const { yes, via = "web" } = body as Answer;
      m.body.state = yes ? { state: "approved", by: "dana", at: now(), via } : { state: "denied", by: "dana", at: now(), via };
      answered(d, m.body, Boolean(yes), m.task);
      const { kind: _, ...view } = m.body;
      return ok(view);
    }
    return no(404, "That question isn’t waiting any more.");
  }

  const route = /^\/agents\/([a-z0-9][a-z0-9-]{0,39})(\/[a-z-]+)?$/.exec(path);
  if (!route) return no(404, "There’s nothing here.");
  const d = find(route[1] ?? "");
  if (!d) return no(404, `There’s no agent called “${route[1]}”.`);
  const id = d.agent.id;
  const action = route[2] ?? "";

  if (action === "" && method === "GET") return ok(d);
  if (action === "/screen" && method === "GET") return ok(screen(id, Number(query.get("since") ?? 0)));
  if (method !== "POST") return no(405, "That isn’t something you can do here.");

  switch (action) {
    case "/ask": {
      const ask = body as Ask;
      const text = (ask.text ?? "").trim();
      if (!text) return no(400, "Say what you’d like it to do.");
      if (d.agent.status === "taken_over") return no(409, `You have ${d.agent.name}’s computer. Hand it back first, then ask.`);
      const task = `t-${next++}`;
      say(d, "person", task, { kind: "text", text });
      record(d, `You asked: ${text.length > 60 ? `${text.slice(0, 57)}…` : text}`);
      const work = () => {
        d.agent.status = "working";
        d.agent.doing = `Working on: ${text.length > 50 ? `${text.slice(0, 47)}…` : text}`;
        print(id, `${prompt(id, "agent")}# ${text}\n`);
        bump(d);
        later(1400, () => {
          say(d, "agent", task, { kind: "progress", steps: ["Read what you asked", "Looked through what’s in my computer"] });
          print(id, "Reading notes and files…\n");
          bump(d);
        });
        later(3200, () => {
          say(d, "agent", task, { kind: "text", text: world.replies[id] ?? "Done. I’ll post here if I need anything from you." });
          record(d, "Answered your question");
          print(id, `Answered.\n${prompt(id, "agent")}`);
          settle(d, "Answered your question");
          bump(d);
        });
      };
      if (d.agent.computer.state === "asleep") wake(d, work);
      else work();
      bump(d);
      return ok({ task });
    }
    case "/take-over": {
      if (d.agent.computer.state === "asleep") return no(409, `${d.agent.name} is asleep. Wake it first.`);
      if (d.agent.status === "taken_over") return ok(d.agent);
      before.set(id, { status: d.agent.status, doing: d.agent.doing });
      d.agent.status = "taken_over";
      d.agent.doing = "Paused: you have its computer";
      record(d, "You took over its computer");
      print(id, `\n--- Dana Alvarez has the keyboard. ${d.agent.name} waits. ---\n${prompt(id, "dana")}`);
      bump(d);
      return ok(d.agent);
    }
    case "/hand-back": {
      if (d.agent.status !== "taken_over") return no(409, "You don’t have its computer.");
      const was = before.get(id) ?? { status: "idle", doing: "Ready" };
      d.agent.status = was.status;
      d.agent.doing = was.doing;
      record(d, "You handed its computer back");
      print(id, `\n--- Handed back to ${d.agent.name}. ---\n${prompt(id, "agent")}`);
      bump(d);
      return ok(d.agent);
    }
    case "/input": {
      if (d.agent.status !== "taken_over") return no(409, "Take over its computer first: the agent is using it.");
      const line = String((body as Input).line ?? "");
      print(id, `${line}\n${run(d, line)}${prompt(id, "dana")}`);
      record(d, `You ran: ${line.length > 50 ? `${line.slice(0, 47)}…` : line}`);
      bump(d);
      // As glass-agents does: the whole screen, from its start.
      return ok(screen(id, 0));
    }
    case "/undo": {
      const done = [...d.messages].reverse().find((m) => m.body.kind === "done" && m.body.undoable);
      if (!done || done.body.kind !== "done") return no(409, "There’s nothing left to undo.");
      done.body.undoable = false;
      const files = done.body.changed ?? [];
      const names = files.map((f) => f.slice(f.lastIndexOf("/") + 1));
      say(d, "agent", done.task, {
        kind: "note",
        text: `Undone: ${names.length ? `${names.join(", ")} ${names.length === 1 ? "is" : "are"} gone, and ` : ""}my computer is back as it was before this task.`,
      });
      record(d, `You undid the task${names.length ? `: removed ${names.join(", ")}` : ""}`);
      print(id, `\n--- undone: back to the snapshot from before this task ---\n${prompt(id, "agent")}`);
      bump(d);
      const result: UndoResult = { text: `Undone. Its computer${files.length ? " and your files are" : " is"} back as they were before this task.` };
      return ok(result);
    }
    case "/wake": {
      if (d.agent.computer.state !== "asleep") return no(409, `${d.agent.name} is already awake.`);
      wake(d);
      return ok(d.agent);
    }
    case "/sleep": {
      if (d.agent.status === "taken_over") return no(409, "Hand its computer back first.");
      if (d.agent.computer.state === "asleep" || d.agent.computer.state === "saving") return no(409, `${d.agent.name} is already asleep.`);
      d.agent.computer.state = "saving";
      d.agent.status = "idle";
      d.agent.doing = "Going to sleep";
      bump(d);
      later(1500, () => {
        d.agent.computer.state = "asleep";
        d.agent.computer.stored = { bytes: Math.round((d.agent.computer.disk_bytes ?? 300_000_000) * 0.35), at: now(), place: "S3: acme-glasshouse-agents" };
        d.agent.status = "asleep";
        d.agent.doing = "Asleep, saved to storage";
        say(d, "agent", d.messages[d.messages.length - 1]?.task ?? "t", {
          kind: "note",
          text: "I went to sleep. My computer is saved in your company’s storage and picks up where it left off when you ask me something.",
        });
        record(d, "Went to sleep; its computer saved to storage");
        bump(d);
      });
      return ok(d.agent);
    }
  }
  return no(404, "There’s nothing here.");
}

function screen(id: string, since: number): Screen {
  const d = find(id);
  const all = screens.get(id) ?? Buffer.alloc(0);
  const from = since > all.length || since < 0 ? all.length : since;
  return {
    offset: all.length,
    text: all.subarray(from).toString(),
    taken_over: d?.agent.status === "taken_over",
    state: d?.agent.computer.state ?? "new",
  };
}

function add(body: NewAgent): Reply {
  const template = world.templates.find((t) => t.id === body.template);
  if (!template) return no(404, "IT hasn’t approved that agent for you.");
  const name = (body.name ?? "").trim() || template.name;
  if (name.length > 60) return no(400, "Give it a shorter name.");
  const base = name.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 36) || template.id;
  let id = base;
  for (let n = 2; find(id); n++) id = `${base}-${n}`;
  const d: AgentDetail = {
    agent: {
      id,
      name,
      role: template.team,
      template: template.id,
      status: "starting",
      doing: "Starting its computer",
      updated_at: now(),
      pending: 0,
      computer: { state: "starting", kind: "Linux namespaces", contained: true, disk_bytes: 0 },
    },
    reach: [...template.reach, { text: "Your passwords", sub: "never inside its computer: it asks the system to act for it", ok: false }],
    messages: [],
    record: [{ at: now(), text: "You added it; its computer is starting" }],
  };
  world.agents.push(d);
  screens.set(id, Buffer.from("Starting its computer…\r\n"));
  bump(d);
  later(2000, () => {
    d.agent.computer.state = "running";
    d.agent.computer.disk_bytes = 310_000_000;
    say(d, "agent", "t-hello", { kind: "text", text: "Hi. My computer is ready. What should I start on?" });
    print(id, `Ready.\n${prompt(id, "agent")}`);
    settle(d, "Ready for its first task");
    bump(d);
  });
  return ok(d.agent, 201);
}

function events(since: number, req: IncomingMessage): Promise<Reply> | Reply {
  const answer = (): Reply => ok({ cursor, agents: [...new Set(changes.filter((c) => c.cursor > since).map((c) => c.agent))] });
  if (since !== cursor) return answer();
  return new Promise((resolve) => {
    const done = () => {
      clearTimeout(timer);
      waiters.delete(done);
      resolve(answer());
    };
    const timer = setTimeout(done, LONG_POLL_MS);
    waiters.add(done);
    req.on("close", () => {
      clearTimeout(timer);
      waiters.delete(done);
    });
  });
}

// --- HTTP ------------------------------------------------------------------

const TYPES: Record<string, string> = { ".html": "text/html; charset=utf-8", ".js": "text/javascript; charset=utf-8", ".css": "text/css; charset=utf-8" };

function send(res: ServerResponse, status: number, type: string, body: string | Buffer): void {
  res.writeHead(status, {
    "content-type": type,
    "content-security-policy": CSP,
    "x-content-type-options": "nosniff",
    "referrer-policy": "no-referrer",
    "cache-control": "no-store",
  });
  res.end(body);
}

async function readBody(req: IncomingMessage): Promise<string> {
  let size = 0;
  const chunks: Buffer[] = [];
  for await (const chunk of req) {
    size += (chunk as Buffer).length;
    if (size > 64 * 1024) throw new Error("too big");
    chunks.push(chunk as Buffer);
  }
  return Buffer.concat(chunks).toString();
}

const server = createServer(async (req, res) => {
  const url = new URL(req.url ?? "/", `http://${req.headers.host ?? "localhost"}`);
  const json = (r: Reply) => send(res, r.status, "application/json", JSON.stringify(r.body));
  try {
    if (url.pathname === "/dev/sign-out" || url.pathname === "/dev/sign-in") {
      signedOut = url.pathname === "/dev/sign-out";
      res.writeHead(303, { location: "/" });
      return res.end();
    }
    if (url.pathname.startsWith("/api/v1/")) {
      const origin = req.headers.origin;
      if (origin && origin !== `http://${req.headers.host}`) return json(no(403, "Not from this site."));
      let body: unknown = undefined;
      if (req.method === "POST") {
        if (!String(req.headers["content-type"] ?? "").startsWith("application/json")) return json(no(415, "Send JSON."));
        try {
          body = JSON.parse((await readBody(req)) || "{}");
        } catch {
          return json(no(400, "That wasn’t JSON."));
        }
      }
      return json(await api(req.method ?? "GET", url.pathname.slice("/api/v1".length), url.searchParams, body, req));
    }
    // The page's own routes are the page, as glass-agents serves them.
    const page = url.pathname === "/" || url.pathname === "/new" || url.pathname.startsWith("/agents/");
    const file = page ? "index.html" : url.pathname.slice(1);
    if (!/^(index\.html|app\.js|app\.css)$/.test(file)) return send(res, 404, "text/plain", "Not here.");
    const ext = file.slice(file.lastIndexOf("."));
    return send(res, 200, TYPES[ext] ?? "application/octet-stream", await readFile(join(DIST, file)));
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return send(res, 500, "text/plain", "No build yet: run `node build.mjs` in apps/ first.");
    return json(no(500, "Something went wrong in the dev server."));
  }
});

server.listen(PORT, "127.0.0.1", () => {
  console.log(`Glasshouse Agents (dev stand-in for glass-agents): http://127.0.0.1:${PORT}`);
});
