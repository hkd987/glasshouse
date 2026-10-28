// What the web app says about agents (ADR 0065 §12, ADR 0066), without a
// DOM: status words, the home screen's tabs and summary, how a
// conversation groups, answers, times and the page's routes.

import assert from "node:assert/strict";
import { test } from "node:test";

import { type AgentStatus, type AgentView, type ComputerState, type Message, validId } from "../src/api.ts";
import {
  ago,
  answerLine,
  computerWords,
  dayLabel,
  groupMessages,
  initials,
  parseRoute,
  size,
  sleepAction,
  splitPath,
  spoken,
  statusWords,
  summary,
  tabs,
  undoable,
  visible,
  waitingLabel,
  whenLine,
} from "../src/model.ts";
import { fixture, pendingOf } from "./fixture.ts";

// Tuesday 27 October 2026, 10:58 local time: the fixture's day.
const NOW = new Date(2026, 9, 27, 10, 58);
const world = fixture(NOW);
const views = world.agents.map((d) => d.agent);
const agent = (status: AgentStatus, extra: Partial<AgentView> = {}): AgentView => ({ ...views[0]!, status, ...extra });
/** The base system's name, which people never see (BR-1), spelled so a
 * search of the source for it finds none. */
const BASE = new RegExp(["free", "bsd"].join(""), "i");

test("adr0066 every status reads in plain words, with a tone for its chip", () => {
  const all: AgentStatus[] = ["starting", "working", "needs_you", "taken_over", "idle", "asleep"];
  for (const status of all) assert.ok(statusWords(status).label.length > 0, status);
  assert.deepEqual(statusWords("needs_you"), { label: "Needs you", tone: "needs" });
  assert.deepEqual(statusWords("working"), { label: "Working", tone: "working" });
  assert.deepEqual(statusWords("asleep"), { label: "Asleep", tone: "asleep" });
  assert.equal(statusWords("taken_over").label, "You have it");
  // A status the page doesn't know yet still reads as something.
  assert.equal(statusWords("dreaming" as AgentStatus).label, "Unknown");
});

test("adr0066 home: the summary, the tabs and what each shows", () => {
  assert.equal(summary(views), "3 agents: 2 need you, 1 asleep");
  assert.equal(summary([]), "No agents yet. Add one your IT team approved.");
  assert.equal(summary([agent("working"), agent("taken_over"), agent("starting"), agent("idle")]), "4 agents: 3 working, 1 idle");
  assert.equal(summary([agent("needs_you")]), "1 agent: 1 needs you");

  // Idle shows only when an agent is idle.
  assert.deepEqual(
    tabs(views, "all").map((t) => `${t.label} ${t.count}`),
    ["All 3", "Needs you 2", "Working 0", "Asleep 1"],
  );
  assert.deepEqual(
    tabs([...views, agent("idle")], "all").map((t) => t.tab),
    ["all", "needs", "working", "idle", "asleep"],
  );
  assert.deepEqual(
    visible(views, "needs").map((a) => a.id),
    ["reconciler", "contract-reviewer"],
  );
  assert.equal(visible(views, "all").length, 3);
});

test("adr0066 the header says how many agents wait on the person, not how many questions", () => {
  const pending = pendingOf(world);
  assert.equal(pending.length, 2);
  assert.equal(waitingLabel(pending), "2 need you");
  assert.equal(waitingLabel([pending[0]!, { ...pending[0]!, approval: { ...pending[0]!.approval, id: "other" } }]), "1 needs you");
  assert.equal(waitingLabel([]), null);
});

test("adr0066 a card's last line and its computer's state", () => {
  assert.equal(whenLine(agent("working"), NOW), "Working now");
  assert.equal(whenLine(views[0]!, NOW), "4 minutes ago");
  assert.equal(whenLine(views[2]!, NOW), "2 hours ago");
  const states: ComputerState[] = ["new", "starting", "ready", "running", "saving", "asleep", "waking"];
  for (const state of states) {
    const words = computerWords({ state, kind: "Linux namespaces", contained: true });
    assert.ok(words.short && words.line, state);
    assert.doesNotMatch(words.line, /namespace|firecracker|vm\b/i, "plain words, no technology");
    assert.doesNotMatch(words.line, BASE, "BR-1");
  }
  assert.equal(sleepAction({ state: "running", kind: "", contained: true }), "sleep");
  assert.equal(sleepAction({ state: "asleep", kind: "", contained: true }), "wake");
  assert.equal(sleepAction({ state: "saving", kind: "", contained: true }), null);
});

test("adr0065 an answered question says who answered, where and when (§13)", () => {
  const clock = (iso: string) => iso.slice(11, 16);
  assert.equal(answerLine({ state: "pending" }, "dana", clock), null);
  assert.equal(answerLine({ state: "approved", by: "dana", at: "2026-10-27T10:58:00Z", via: "teams" }, "dana", clock), "You approved in Teams at 10:58.");
  assert.equal(answerLine({ state: "approved", by: "dana", at: "2026-10-27T10:58:00Z", via: "web" }, "dana", clock), "You approved at 10:58.");
  assert.equal(
    answerLine({ state: "denied", by: "sam", at: "2026-10-27T11:02:00Z", via: "desktop" }, "dana", clock),
    "sam said no on the desktop at 11:02. Nothing was done.",
  );
  assert.equal(answerLine({ state: "expired" }, "dana", clock), "No answer in time. Nothing was done.");
});

test("adr0066 a conversation groups into days and runs, with a task's progress as one card", () => {
  const reconciler = world.agents[0]!.messages;
  const rows = groupMessages(reconciler, NOW);
  assert.deepEqual(
    rows.map((r) => (r.kind === "day" ? `day:${r.label}` : `${r.from}:${r.items.map((m) => m.body.kind).join("+")}`)),
    // m2 is 7 minutes before the progress: a run of its own. The two
    // progress messages, a minute apart, read as one card.
    ["day:Today", "person:text", "agent:text", "agent:progress+done+approval"],
  );
  const run = rows[3];
  assert.ok(run && run.kind === "run");
  const progress = run.items[0]!.body;
  assert.ok(progress.kind === "progress");
  assert.deepEqual(progress.steps, ["Read 214 invoices", "Matched 207 to bank payments", "Checked the 7 that don’t match"]);
  // The run's time is its last message's.
  assert.equal(run.at, reconciler[reconciler.length - 1]!.at);
  // The messages themselves are left as they were.
  assert.equal(reconciler[2]!.body.kind === "progress" && reconciler[2]!.body.steps.length, 2);
});

test("adr0066 runs break on a new day, a new task, the other side and a long gap", () => {
  const m = (id: string, at: Date, from: Message["from"], task: string): Message => ({ id, at: at.toISOString(), from, task, body: { kind: "text", text: id } });
  const at = (d: number, h: number, min: number) => new Date(2026, 9, d, h, min);
  const rows = groupMessages(
    [
      m("a", at(26, 17, 0), "agent", "t1"),
      m("b", at(27, 9, 0), "agent", "t1"), // the next day
      m("c", at(27, 9, 2), "agent", "t1"), // joins b
      m("d", at(27, 9, 3), "agent", "t2"), // another task
      m("e", at(27, 9, 4), "person", "t2"), // the person
      m("f", at(27, 9, 20), "person", "t2"), // 16 minutes on
    ],
    NOW,
  );
  assert.deepEqual(
    rows.map((r) => (r.kind === "day" ? r.label : r.items.map((x) => x.id).join(""))),
    ["Yesterday", "a", "Today", "bc", "d", "e", "f"],
  );
});

test("adr0066 undo puts back the newest task that still can be, from when it started", () => {
  const reconciler = world.agents[0]!.messages;
  assert.deepEqual(undoable(reconciler), { task: "t-rec-1", message: "m5", since: reconciler[0]!.at });
  const done = reconciler.map((m) => (m.body.kind === "done" ? { ...m, body: { ...m.body, undoable: false } } : m));
  assert.equal(undoable(done), null);
  assert.equal(undoable([]), null);
});

test("adr0065 a confidential question is said only as needing the person (ADR 0018)", () => {
  const contract = world.agents[1]!;
  const question = contract.messages.find((m) => m.body.kind === "approval")!;
  assert.equal(spoken("Contract reviewer", question), "Contract reviewer needs you.");
  const plain = world.agents[0]!.messages.find((m) => m.body.kind === "approval")!;
  assert.match(spoken("Reconciler", plain), /^Reconciler needs you: Send a reminder/);
  // The person's own words aren't read back to them.
  assert.equal(spoken("Reconciler", world.agents[0]!.messages[0]!), "");
});

test("adr0066 times, names, sizes and paths as people read them", () => {
  assert.equal(ago(new Date(NOW.getTime() - 20_000).toISOString(), NOW), "just now");
  assert.equal(ago(new Date(NOW.getTime() - 60_000).toISOString(), NOW), "1 minute ago");
  assert.equal(ago(new Date(NOW.getTime() - 3 * 3600_000).toISOString(), NOW), "3 hours ago");
  assert.equal(ago(new Date(NOW.getTime() - 30 * 3600_000).toISOString(), NOW), "yesterday");
  assert.equal(ago("not a time", NOW), "");
  assert.equal(dayLabel(new Date(2026, 9, 23, 9), NOW), "Friday");
  assert.equal(dayLabel(new Date(2026, 9, 12, 9), NOW), "12 October");
  assert.equal(dayLabel(new Date(2025, 11, 30, 9), NOW), "30 December 2025");
  assert.equal(initials("Dana Alvarez"), "DA");
  assert.equal(initials("Dana Maria Alvarez"), "DA");
  assert.equal(initials("dana"), "D");
  assert.equal(size(412_000_000), "412 MB");
  assert.equal(size(2_412_000_000), "2.4 GB");
  assert.equal(size(900), "900 bytes");
  assert.deepEqual(splitPath("Finance/March/march.xlsx"), { name: "march.xlsx", folder: "Finance/March" });
  assert.deepEqual(splitPath("notes.txt"), { name: "notes.txt", folder: "" });
});

test("adr0066 the page's addresses are the ones glass-agents serves it at", () => {
  assert.deepEqual(parseRoute("/"), { page: "home" });
  assert.deepEqual(parseRoute("/new"), { page: "new" });
  assert.deepEqual(parseRoute("/agents/reconciler"), { page: "agent", id: "reconciler" });
  assert.deepEqual(parseRoute("/agents/reconciler/"), { page: "agent", id: "reconciler" });
  assert.deepEqual(parseRoute("/agents/reconciler-2/computer"), { page: "computer", id: "reconciler-2" });
  for (const bad of ["/agents/..%2Fx", "/agents/-a", "/agents/Reconciler", "/agents/a/b", "/agents/%E0%A4%A", "/settings"]) {
    assert.deepEqual(parseRoute(bad), { page: "missing" }, bad);
  }
});

test("adr0066 agent ids are checked as agents-api's valid_id does", () => {
  assert.ok(validId("reconciler-2"));
  assert.ok(validId("a".repeat(40)));
  for (const bad of ["../x", "-a", "", "a".repeat(41), "Ab", "a b", "a/b"]) assert.equal(validId(bad), false, bad);
});

test("adr0066 the fixture is in the contract's shapes", () => {
  for (const d of world.agents) {
    const ids = d.messages.map((m) => m.id);
    assert.equal(new Set(ids).size, ids.length, `${d.agent.id}: message ids are unique`);
    const waiting = d.messages.filter((m) => m.body.kind === "approval" && m.body.state.state === "pending").length;
    assert.equal(d.agent.pending, waiting, `${d.agent.id}: pending counts its questions`);
    assert.ok(validId(d.agent.id));
    // Times are RFC 3339, newest last.
    const times = d.messages.map((m) => Date.parse(m.at));
    assert.deepEqual([...times].sort((a, b) => a - b), times);
  }
  assert.doesNotMatch(JSON.stringify(world), BASE, "BR-1");
});
