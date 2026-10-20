// Live editing (DC-3, ADR 0035 §3) without a bridge: two pages' Yjs
// documents through a fake computer that does what `/_bridge/live` does
// (push appends to the doc's log; pull returns what's after a cursor,
// waiting for news), and comments' anchors through concurrent edits.

import assert from "node:assert/strict";
import { test } from "node:test";

import { initProseMirrorDoc, absolutePositionToRelativePosition, relativePositionToAbsolutePosition } from "y-prosemirror";
import * as Y from "yjs";
import { toBase64 } from "lib0/buffer";

import { Bridge, BridgeError, memoryStore, Offline, type Transport } from "../../sdk/src/index.ts";
import { applyState, type Channel, decodeAnchor, encodeAnchor, failureKind, FRAGMENT, LiveSync, type Pulled, REMOTE } from "../src/live.ts";
import { schema } from "../src/schema.ts";

/** The computer's side of one doc: its log of updates, and pulls waiting on it. */
class Computer {
  log: { update: string; by: string; by_name: string }[] = [];
  #waiting = new Set<() => void>();
  /** Sessions that have the doc open; others are refused, as glassd refuses them. */
  open = new Set<string>();
  pushes = 0;

  /** The doc's whole state, as doc.open returns it. */
  state(): string {
    const doc = new Y.Doc();
    for (const u of this.log) Y.applyUpdate(doc, Buffer.from(u.update, "base64"));
    return toBase64(Y.encodeStateAsUpdate(doc));
  }

  channel(login: string, faults: { offline?: number } = {}): Channel & { pulls: number } {
    this.open.add(login);
    let offline = faults.offline ?? 0;
    const refuse = () => {
      if (!this.open.has(login)) throw new BridgeError(409, "This doc isn't open in this session; open it first.");
    };
    const channel = {
      pulls: 0,
      push: async (update: string) => {
        if (offline > 0) {
          offline -= 1;
          throw new Offline();
        }
        refuse();
        this.pushes += 1;
        this.log.push({ update, by: login, by_name: login });
        for (const wake of this.#waiting) wake();
        return { cursor: this.log.length };
      },
      pull: async (since: number, waitSecs: number): Promise<Pulled> => {
        channel.pulls += 1;
        refuse();
        if (this.log.length <= since) {
          await new Promise<void>((resolve) => {
            const timer = setTimeout(done, waitSecs * 1000);
            timer.unref();
            this.#waiting.add(done);
            function done() {
              clearTimeout(timer);
              resolve();
            }
          });
          this.#waiting.clear();
        }
        refuse();
        return { updates: this.log.slice(since), cursor: this.log.length };
      },
      reopen: async () => {
        this.open.add(login);
        return { state: this.state(), cursor: this.log.length };
      },
    };
    return channel;
  }

  close(): void {
    for (const wake of this.#waiting) wake();
  }
}

/** A paragraph of `text` at the end of the doc, as y-prosemirror writes one. */
function paragraph(doc: Y.Doc, text: string): Y.XmlText {
  const p = new Y.XmlElement("paragraph");
  doc.getXmlFragment(FRAGMENT).push([p]);
  const t = new Y.XmlText();
  p.insert(0, [t]);
  t.insert(0, text);
  return t;
}

function texts(doc: Y.Doc): string[] {
  return doc
    .getXmlFragment(FRAGMENT)
    .toArray()
    .map((el) => ((el as Y.XmlElement).toArray()[0] as Y.XmlText | undefined)?.toString() ?? "");
}

async function until(check: () => boolean, ms = 3000): Promise<void> {
  const end = Date.now() + ms;
  while (!check()) {
    if (Date.now() > end) throw new Error("timed out");
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
}

function session(computer: Computer, login: string, more: Partial<ConstructorParameters<typeof LiveSync>[0]> = {}) {
  const doc = new Y.Doc();
  const live = new LiveSync({ doc, channel: computer.channel(login), cursor: 0, intervalMs: 30, waitSecs: 2, retryMs: 20, ...more }).start();
  return { doc, live };
}

test("dc3 two pages editing at once converge through push and pull", async () => {
  const computer = new Computer();
  const priya = session(computer, "priya");
  const sam = session(computer, "sam");
  paragraph(priya.doc, "Agenda from Priya");
  paragraph(sam.doc, "Notes from Sam");
  await until(() => texts(priya.doc).length === 2 && texts(sam.doc).length === 2);
  assert.deepEqual(texts(priya.doc), texts(sam.doc));
  assert.deepEqual([...texts(priya.doc)].sort(), ["Agenda from Priya", "Notes from Sam"]);
  // Typing into the same line from both sides merges too.
  const line = priya.doc.getXmlFragment(FRAGMENT).get(0) as Y.XmlElement;
  (line.get(0) as Y.XmlText).insert(0, "1. ");
  const theirs = sam.doc.getXmlFragment(FRAGMENT).get(0) as Y.XmlElement;
  (theirs.get(0) as Y.XmlText).insert((theirs.get(0) as Y.XmlText).length, " (draft)");
  await until(() => texts(priya.doc)[0] === texts(sam.doc)[0] && texts(sam.doc)[0]?.startsWith("1. ") === true && texts(priya.doc)[0]?.endsWith("(draft)") === true);
  assert.deepEqual(Y.encodeStateVector(priya.doc), Y.encodeStateVector(sam.doc));
  priya.live.stop();
  sam.live.stop();
  computer.close();
});

test("dc3 a burst of typing is merged and pushed at most once per interval", async () => {
  const computer = new Computer();
  const priya = session(computer, "priya", { intervalMs: 400 });
  const sam = session(computer, "sam");
  const t = paragraph(priya.doc, "");
  // Typing for about 600 ms, a key every 20 ms: 30 updates.
  const started = Date.now();
  for (const ch of "Quarterly plan, first draft..") {
    t.insert(t.length, ch);
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
  const typed = Date.now() - started;
  await until(() => texts(sam.doc)[0] === "Quarterly plan, first draft..");
  // The first change goes at once; the rest wait for the interval, merged.
  const most = Math.ceil(typed / 400) + 1;
  assert.ok(computer.pushes >= 2 && computer.pushes <= most, `${computer.pushes} pushes for ${typed} ms of typing (at most ${most})`);
  priya.live.stop();
  sam.live.stop();
  computer.close();
});

test("dc3 what others changed is applied as remote and never pushed back", async () => {
  const computer = new Computer();
  const priya = session(computer, "priya");
  const sam = session(computer, "sam");
  const origins: unknown[] = [];
  sam.doc.on("update", (_u: Uint8Array, origin: unknown) => origins.push(origin));
  paragraph(priya.doc, "Only Priya writes");
  await until(() => texts(sam.doc)[0] === "Only Priya writes");
  await new Promise((resolve) => setTimeout(resolve, 80));
  assert.equal(sam.live.pushes, 0);
  assert.ok(origins.length > 0 && origins.every((o) => o === REMOTE));
  assert.equal(computer.log.filter((u) => u.by === "sam").length, 0);
  priya.live.stop();
  sam.live.stop();
  computer.close();
});

test("dc3 viewers and commenters never push", async () => {
  const computer = new Computer();
  const viewer = session(computer, "lee", { readOnly: true });
  paragraph(viewer.doc, "typed somehow");
  await new Promise((resolve) => setTimeout(resolve, 80));
  assert.equal(viewer.live.pushes, 0);
  assert.equal(computer.log.length, 0);
  viewer.live.stop();
  computer.close();
});

test("dc3 a session glassd no longer has open is opened again and loses nothing", async () => {
  const computer = new Computer();
  const priya = session(computer, "priya");
  const sam = session(computer, "sam");
  paragraph(sam.doc, "Before the restart");
  await until(() => texts(priya.doc).length === 1);
  let reopened = 0;
  const healths: string[] = [];
  priya.live.stop();
  const again = new LiveSync({
    doc: priya.doc,
    channel: computer.channel("priya"),
    cursor: priya.live.cursor,
    intervalMs: 20,
    waitSecs: 2,
    retryMs: 20,
    onReopen: () => (reopened += 1),
    onHealth: (h) => healths.push(h),
  }).start();
  // glassd restarted: no session has the doc open. Both write meanwhile.
  computer.open.clear();
  paragraph(priya.doc, "Written while it wasn't open");
  computer.open.add("sam");
  paragraph(sam.doc, "Sam carries on");
  await until(() => texts(priya.doc).length === 3 && texts(sam.doc).length === 3);
  assert.ok(reopened >= 1);
  assert.deepEqual(texts(priya.doc), texts(sam.doc));
  assert.ok(!healths.includes("refused"), healths.join(","));
  again.stop();
  sam.live.stop();
  computer.close();
});

test("dc3 an unreachable system keeps what's unsent and sends it when back", async () => {
  const computer = new Computer();
  const doc = new Y.Doc();
  const healths: string[] = [];
  const live = new LiveSync({
    doc,
    channel: computer.channel("priya", { offline: 2 }),
    cursor: 0,
    intervalMs: 10,
    waitSecs: 2,
    retryMs: 20,
    onHealth: (h) => healths.push(h),
  }).start();
  const sam = session(computer, "sam");
  paragraph(doc, "Sent after a blip");
  await until(() => texts(sam.doc)[0] === "Sent after a blip");
  assert.ok(healths.includes("offline"), healths.join(","));
  await until(() => live.health === "live");
  assert.equal(live.unsent, false);
  live.stop();
  sam.live.stop();
  computer.close();
});

test("dc3 a refused push stops sending and says why", async () => {
  const doc = new Y.Doc();
  const said: string[] = [];
  const channel: Channel = {
    push: async () => {
      throw new BridgeError(403, "You can view this doc, not edit it.");
    },
    pull: () => new Promise<Pulled>(() => undefined),
    reopen: async () => null,
  };
  const live = new LiveSync({ doc, channel, cursor: 0, intervalMs: 5, onHealth: (h, why) => said.push(`${h}: ${why ?? ""}`) }).start();
  paragraph(doc, "no");
  await until(() => live.health === "refused");
  assert.ok(said.some((s) => s.includes("not edit it")));
  assert.equal(live.stopped, true);
});

test("dc3 failures are told apart by name, so the page needs no DOM", () => {
  assert.equal(failureKind(new Offline()), "offline");
  assert.equal(failureKind(new BridgeError(409, "x")), "not-open");
  assert.equal(failureKind(new BridgeError(400, "docs: cv-1 isn't open in this session")), "not-open");
  assert.equal(failureKind(new BridgeError(403, "Viewers can't edit")), "refused");
  assert.equal(failureKind(new TypeError("network")), "offline");
});

test("dc3 the sdk's live() posts to /_bridge/live with the contract's fields", async () => {
  const sent: { endpoint: string; body: unknown }[] = [];
  const transport: Transport = {
    async post(endpoint, body) {
      sent.push({ endpoint, body });
      if (endpoint !== "live") return { status: 200, body: {} };
      const b = body as { op: string };
      return b.op === "push" ? { status: 200, body: { cursor: 7 } } : { status: 200, body: { updates: [], cursor: 7 } };
    },
  };
  const bridge = new Bridge({ app: "docs", transport, store: memoryStore() });
  assert.deepEqual(await bridge.live({ op: "push", doc: "cv-0", update: "AAA=" }), { cursor: 7 });
  assert.deepEqual(await bridge.live({ op: "pull", doc: "cv-0", since: 3, wait_secs: 25, client: "page-1" }), { updates: [], cursor: 7 });
  assert.deepEqual(sent, [
    { endpoint: "live", body: { op: "push", doc: "cv-0", update: "AAA=" } },
    { endpoint: "live", body: { op: "pull", doc: "cv-0", since: 3, wait_secs: 25, client: "page-1" } },
  ]);
  const refusing: Transport = { post: async () => ({ status: 409, body: { error: "This doc isn't open in this session." } }) };
  await assert.rejects(new Bridge({ app: "docs", transport: refusing, store: memoryStore() }).live({ op: "pull", doc: "cv-0", since: 0, wait_secs: 25 }), (e: unknown) => failureKind(e) === "not-open");
});

// --- Comments' anchors -------------------------------------------------------

/** Where `word` starts in a ProseMirror doc made from the fragment, and the mapping y-prosemirror keeps. */
function pm(doc: Y.Doc) {
  const { doc: node, mapping } = initProseMirrorDoc(doc.getXmlFragment(FRAGMENT), schema);
  const find = (word: string) => {
    let at = -1;
    node.descendants((child, pos) => {
      if (at < 0 && child.isText && child.text?.includes(word)) at = pos + (child.text?.indexOf(word) ?? 0);
    });
    return at;
  };
  return { node, mapping, find };
}

test("dc3 a comment's anchor survives concurrent edits on both sides", async () => {
  const computer = new Computer();
  const priya = session(computer, "priya");
  const sam = session(computer, "sam");
  paragraph(priya.doc, "Hello world, see the totals");
  await until(() => texts(sam.doc).length === 1);

  // Priya comments on "world" (ADR 0035 §2: a relative position, base64).
  const a = pm(priya.doc);
  const at = a.find("world");
  const path = encodeAnchor(absolutePositionToRelativePosition(at, priya.doc.getXmlFragment(FRAGMENT), a.mapping as never));
  assert.match(path, /^[A-Za-z0-9+/]+={0,2}$/);

  // Meanwhile: Priya types before it, Sam deletes "Hello " and adds a line above.
  const mine = (priya.doc.getXmlFragment(FRAGMENT).get(0) as Y.XmlElement).get(0) as Y.XmlText;
  mine.insert(0, "Well: ");
  const theirs = (sam.doc.getXmlFragment(FRAGMENT).get(0) as Y.XmlElement).get(0) as Y.XmlText;
  theirs.delete(0, 6);
  const above = new Y.XmlElement("heading");
  above.setAttribute("level", 2 as never);
  sam.doc.getXmlFragment(FRAGMENT).insert(0, [above]);
  const title = new Y.XmlText();
  above.insert(0, [title]);
  title.insert(0, "Weekly sync");
  await until(() => texts(priya.doc).length === 2 && texts(priya.doc)[1] === texts(sam.doc)[1] && texts(sam.doc)[1]?.startsWith("Well: ") === true);

  for (const doc of [priya.doc, sam.doc]) {
    const b = pm(doc);
    const relative = decodeAnchor(path);
    assert.ok(relative);
    const pos = relativePositionToAbsolutePosition(doc, doc.getXmlFragment(FRAGMENT), relative, b.mapping as never);
    assert.equal(pos, b.find("world"));
    assert.equal(b.node.textBetween(pos as number, (pos as number) + 5), "world");
  }
  priya.live.stop();
  sam.live.stop();
  computer.close();
});

test("dc3 an anchor whose text was deleted still resolves inside the doc", () => {
  const doc = new Y.Doc();
  const t = paragraph(doc, "Remove this sentence entirely. Keep this.");
  const { mapping, find } = pm(doc);
  const path = encodeAnchor(absolutePositionToRelativePosition(find("this sentence"), doc.getXmlFragment(FRAGMENT), mapping as never));
  t.delete(0, "Remove this sentence entirely. ".length);
  const after = pm(doc);
  const relative = decodeAnchor(path);
  assert.ok(relative);
  const pos = relativePositionToAbsolutePosition(doc, doc.getXmlFragment(FRAGMENT), relative, after.mapping as never);
  assert.ok(pos !== null && pos >= 0 && pos <= after.node.content.size, String(pos));
});

test("dc3 anchors that aren't base64 positions are ignored, never thrown", () => {
  for (const bad of [null, undefined, "", "not base64!", "AAAA", "x".repeat(5000), 42]) {
    const decoded = decodeAnchor(bad);
    // "AAAA" decodes to bytes but not to a position.
    assert.ok(decoded === null || decoded instanceof Y.RelativePosition, String(bad));
  }
});

test("dc3 doc.open's state applies as remote and twice is the same as once", () => {
  const source = new Y.Doc();
  paragraph(source, "From the computer");
  const state = toBase64(Y.encodeStateAsUpdate(source));
  const doc = new Y.Doc();
  const origins: unknown[] = [];
  doc.on("update", (_u: Uint8Array, origin: unknown) => origins.push(origin));
  applyState(doc, state);
  applyState(doc, state);
  assert.deepEqual(texts(doc), ["From the computer"]);
  assert.ok(origins.every((o) => o === REMOTE));
});
