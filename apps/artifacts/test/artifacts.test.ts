// Artifacts' logic (AR-1 to AR-5, ADR 0033): each artifact's own origin,
// what a frame may tell the app (every field untrusted), which comments
// get pins and where, and what a share sends.

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  type Comment,
  type Row,
  artifactUrl,
  deepLink,
  labelName,
  MAX_RECTS,
  partition,
  pinned,
  pinPosition,
  readable,
  readFrameMessage,
  shareInput,
  threads,
  tooLarge,
  updateInput,
  versionBadge,
  wherePaths,
} from "../src/artifacts.ts";

const HEX = "0123456789abcdef0123456789abcdef";
const ID = `cv-${HEX}`;
const here = { protocol: "http:", host: "artifacts.localhost:7300" };

function comment(over: Partial<Comment> & { id: string }): Comment {
  return {
    by: "priya",
    by_name: "Priya Shah",
    attribution: null,
    text: "Totals look off",
    version: 2,
    anchor: null,
    reply_to: null,
    resolved: false,
    at: "2026-10-20T09:00:00Z",
    ...over,
  };
}

function row(over: Partial<Row> & { id: string }): Row {
  return {
    title: over.id,
    owner: "priya",
    owner_name: "Priya Shah",
    mine: false,
    people: [],
    version: 1,
    updated_at: "2026-10-20T09:00:00Z",
    open_comments: 0,
    ...over,
  };
}

test("ar2 an artifact's page is on an origin of its own, the app's port, latest or a version", () => {
  assert.equal(artifactUrl(here, ID), `http://${HEX}.artifact.localhost:7300/`);
  assert.equal(artifactUrl(here, ID, 3), `http://${HEX}.artifact.localhost:7300/v3`);
  assert.equal(artifactUrl(here, ID, null), `http://${HEX}.artifact.localhost:7300/`);
  for (const bad of [0, -1, 1.5]) assert.equal(artifactUrl(here, ID, bad), `http://${HEX}.artifact.localhost:7300/`, String(bad));
  // Only an artifact's id, and only from an app's host.
  for (const id of ["cv-123", `cv-${HEX.toUpperCase()}`, `${ID}/../x`, `x${ID}`, HEX, `cv-${HEX}.evil.example`]) {
    assert.equal(artifactUrl(here, id), null, id);
  }
  assert.equal(artifactUrl({ protocol: "http:", host: "127.0.0.1:7300" }, ID), null);
  assert.equal(artifactUrl({ protocol: "http:", host: "mail.localhost:7311" }, ID), `http://${HEX}.artifact.localhost:7311/`);
});

test("ar1 a link names one artifact by its hash", () => {
  assert.equal(deepLink(`#${ID}`), ID);
  assert.equal(deepLink(ID), ID);
  for (const bad of ["", "#", "#cv-1", `#${ID}x`, `#${ID}&x=1`]) assert.equal(deepLink(bad), null, bad);
});

test("ar5 what was shared with the person and their own, each newest first", () => {
  const { shared, mine } = partition([
    row({ id: "old", updated_at: "2026-10-18T09:00:00Z" }),
    row({ id: "mine", mine: true, owner: "me" }),
    row({ id: "new", updated_at: "2026-10-20T10:00:00Z" }),
    row({ id: "none", updated_at: null }),
  ]);
  assert.deepEqual(
    shared.map((r) => r.id),
    ["new", "old", "none"],
  );
  assert.deepEqual(
    mine.map((r) => r.id),
    ["mine"],
  );
});

test("ar2 the app hears only the pin script's messages, every field checked", () => {
  // Anything not tagged, not an object, or of another type: dropped.
  for (const bad of [null, "ready", 7, [], { type: "ready" }, { glass: "other", type: "ready" }, { glass: "artifact", type: "navigate" }]) {
    assert.equal(readFrameMessage(bad), null, JSON.stringify(bad));
  }
  assert.deepEqual(readFrameMessage({ glass: "artifact", type: "ready", height: 900 }), { type: "ready", height: 900 });
  assert.deepEqual(readFrameMessage({ glass: "artifact", type: "ready", height: "tall" }), { type: "ready", height: 0 });

  // A pick: a path of at most 512 characters or none, a point in thousandths.
  assert.deepEqual(readFrameMessage({ glass: "artifact", type: "pick", path: "main:nth-of-type(1)", x: 120.4, y: 2400 }), {
    type: "pick",
    path: "main:nth-of-type(1)",
    x: 120,
    y: 1000,
  });
  assert.deepEqual(readFrameMessage({ glass: "artifact", type: "pick", path: null, x: -5, y: 3 }), { type: "pick", path: null, x: 0, y: 3 });
  for (const bad of [
    { path: "x".repeat(513), x: 1, y: 1 },
    { path: "", x: 1, y: 1 },
    { path: "a\nb", x: 1, y: 1 },
    { path: 7, x: 1, y: 1 },
    { path: "p", x: Number.NaN, y: 1 },
    { path: "p", x: Number.POSITIVE_INFINITY, y: 1 },
    { path: "p", x: "1", y: 1 },
  ]) {
    assert.equal(readFrameMessage({ glass: "artifact", type: "pick", ...bad }), null, JSON.stringify(bad));
  }

  // Rects: at most 200, each checked, a bad one null.
  const many = Array.from({ length: 500 }, (_, i) => ({ path: `p${i}`, x: i, y: 0, w: 10, h: 10 }));
  const heard = readFrameMessage({ glass: "artifact", type: "rects", rects: many });
  assert.equal(heard?.type, "rects");
  assert.equal(heard?.type === "rects" && heard.rects.length, MAX_RECTS);
  const checked = readFrameMessage({
    glass: "artifact",
    type: "rects",
    rects: [null, { path: "a", x: 1e12, y: -1e12, w: -4, h: 5 }, { path: "b", x: Number.NaN, y: 0, w: 1, h: 1 }, "x", { x: 0, y: 0, w: 1, h: 1 }],
  });
  assert.deepEqual(checked, { type: "rects", rects: [null, { path: "a", x: 100_000, y: -100_000, w: 0, h: 5 }, null, null, null] });
  assert.equal(readFrameMessage({ glass: "artifact", type: "rects", rects: "all" }), null);
});

test("ar3 pins go on the shown version's open comments at an element, numbered in order", () => {
  const comments = [
    comment({ id: "a", anchor: { path: "main > table:nth-of-type(1)" } }),
    comment({ id: "b", anchor: { x: 10, y: 20 } }),
    comment({ id: "c", anchor: { path: "h1:nth-of-type(1)" }, resolved: true }),
    comment({ id: "d", anchor: { path: "h1:nth-of-type(1)" }, version: 1 }),
    comment({ id: "e", anchor: { path: "p:nth-of-type(2)" }, reply_to: "a" }),
    comment({ id: "f", anchor: { path: "x".repeat(600) } }),
    comment({ id: "g", anchor: { path: "p:nth-of-type(3)" } }),
  ];
  const pins = pinned(comments, 2);
  assert.deepEqual(
    pins.map((p) => [p.n, p.comment.id]),
    [
      [1, "a"],
      [2, "g"],
    ],
  );
  // v1's comment gets its pin on v1.
  assert.deepEqual(
    pinned(comments, 1).map((p) => p.comment.id),
    ["d"],
  );
  // The frame hears paths and nothing else: no comment text.
  const paths = wherePaths(pins, "body > p:nth-of-type(9)");
  assert.deepEqual(paths, ["main > table:nth-of-type(1)", "p:nth-of-type(3)", "body > p:nth-of-type(9)"]);
  assert.ok(!JSON.stringify(paths).includes("Totals"));
  assert.deepEqual(wherePaths([...pins, ...pins], "p:nth-of-type(3)"), ["main > table:nth-of-type(1)", "p:nth-of-type(3)"]);
});

test("ar3 a pin sits at its element's corner inside the frame, and not when scrolled away", () => {
  const frame = { w: 800, h: 600 };
  assert.deepEqual(pinPosition({ path: "p", x: 100, y: 200, w: 50, h: 20 }, frame), { left: 89, top: 189 });
  assert.deepEqual(pinPosition({ path: "p", x: 0, y: 0, w: 50, h: 20 }, frame), { left: 0, top: 0 });
  assert.deepEqual(pinPosition({ path: "p", x: 795, y: 598, w: 50, h: 20 }, frame), { left: 778, top: 578 });
  assert.equal(pinPosition({ path: "p", x: 10, y: -100, w: 50, h: 20 }, frame), null);
  assert.equal(pinPosition({ path: "p", x: 10, y: 700, w: 50, h: 20 }, frame), null);
  assert.equal(pinPosition({ path: "p", x: 900, y: 10, w: 50, h: 20 }, frame), null);
});

test("ar3 ar4 replies sit under their comment, and older versions' comments say so", () => {
  const all = [comment({ id: "a" }), comment({ id: "b", reply_to: "a" }), comment({ id: "c", reply_to: "gone" }), comment({ id: "d", reply_to: "a" })];
  assert.deepEqual(
    threads(all).map((t) => [t.comment.id, t.replies.map((r) => r.id)]),
    [
      ["a", ["b", "d"]],
      ["c", []],
    ],
  );
  assert.equal(versionBadge(comment({ id: "x", version: 1 }), 2), "on v1");
  assert.equal(versionBadge(comment({ id: "x", version: 2 }), 2), null);
});

test("ar1 a share sends a title, the page and its audience, and fits the bridge", () => {
  const ok = shareInput({ title: "  Q3 forecast ", html: "<h1>Q3</h1>", with: ["priya", "priya"], conversation: null, note: " For Friday " });
  assert.deepEqual(ok, { input: { title: "Q3 forecast", html: "<h1>Q3</h1>", with: ["priya"], note: "For Friday" } });
  const labeled = shareInput({ title: "Q3", label: "confidential", html: "<p>", with: [], conversation: ID, note: "" });
  assert.deepEqual(labeled, { input: { title: "Q3", html: "<p>", with: [], label: "confidential", conversation: ID } });
  assert.ok("problem" in shareInput({ title: "Q3", label: "restricted", html: "<p>", with: [], conversation: null, note: "" }));
  for (const [form, why] of [
    [{ title: " ", html: "<p>" }, "title"],
    [{ title: "a\nb", html: "<p>" }, "one line"],
    [{ title: "x".repeat(201), html: "<p>" }, "200"],
    [{ title: "Q3", html: "  " }, ".html"],
    [{ title: "Q3", html: "<p>", conversation: "general" }, "conversation"],
  ] as const) {
    const result = shareInput({ with: [], conversation: null, note: "", ...form });
    assert.ok("problem" in result && result.problem.includes(why), `${JSON.stringify(form)}: ${JSON.stringify(result)}`);
  }
  // A page up to the blob limit fits the bridge; one whose escaping makes
  // the request too large for it says so before sending.
  const big = shareInput({ title: "Big", html: "x".repeat(1400 * 1024), with: [], conversation: null, note: "" });
  assert.ok("input" in big, JSON.stringify(big).slice(0, 200));
  const escaped = shareInput({ title: "Big", html: "\u0001".repeat(800 * 1024), with: [], conversation: null, note: "" });
  assert.ok("problem" in escaped && escaped.problem.includes("up to 4 MB"), JSON.stringify(escaped).slice(0, 200));
  const huge = tooLarge("artifact.share", { html: "x".repeat(1600 * 1024) });
  assert.ok(huge?.includes("1.5 MB"), huge ?? "");
  assert.equal(tooLarge("artifact.share", { html: "<p>small</p>" }), null);

  assert.deepEqual(updateInput(ID, "<p>v2</p>", " Fixed totals "), { input: { artifact: ID, html: "<p>v2</p>", note: "Fixed totals" } });
  assert.ok("problem" in updateInput(ID, "<p>v2</p>", " "));
});

test("br1 what results say reaches the screen without the base system's name", () => {
  assert.equal(readable("FreeBSD jail refused it"), "the system jail refused it");
  assert.equal(readable("on freebsd 15"), "on the system 15");
  assert.equal(readable("Shared Q3 with Priya Shah."), "Shared Q3 with Priya Shah.");
  assert.equal(labelName("confidential"), "Confidential");
  assert.equal(labelName("<b>"), null);
});
