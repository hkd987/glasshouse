// Docs' logic (DC-1, DC-3, DC-4, ADR 0035): ids and links, who may do what,
// the list, and what create, roles, comments and versions send.

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  type Comment,
  type Row,
  anchored,
  artifactFrom,
  artifactsUrl,
  canComment,
  canEdit,
  commentInput,
  createInput,
  deepLink,
  linkFrom,
  partition,
  readable,
  roleName,
  rolesInput,
  safeHref,
  slashMatches,
  snippet,
  threads,
  versionInput,
  wordAt,
} from "../src/docs.ts";

const HEX = "0123456789abcdef0123456789abcdef";
const ID = `cv-${HEX}`;

function row(over: Partial<Row> & { id: string }): Row {
  return { title: over.id, owner: "priya", owner_name: "Priya Shah", mine: false, role: "editor", people: [], updated_at: "2026-10-20T09:00:00Z", ...over };
}

function comment(over: Partial<Comment> & { id: string }): Comment {
  return { by: "priya", by_name: "Priya Shah", attribution: null, text: "Check this", anchor: null, reply_to: null, resolved: false, at: "2026-10-20T09:00:00Z", ...over };
}

test("dc1 a link names one doc by its hash", () => {
  assert.equal(deepLink(`#${ID}`), ID);
  assert.equal(deepLink(ID), ID);
  for (const bad of ["", "#", "#cv-1", `#${ID}x`, `#${ID}&x=1`, `#cv-${HEX.toUpperCase()}`]) assert.equal(deepLink(bad), null, bad);
});

test("dc4 shared with me and mine, each newest first", () => {
  const { shared, mine } = partition([
    row({ id: "a", updated_at: "2026-10-19T09:00:00Z" }),
    row({ id: "b", mine: true, role: "owner" }),
    row({ id: "c", updated_at: "2026-10-20T10:00:00Z" }),
    row({ id: "d", updated_at: null }),
  ]);
  assert.deepEqual(
    shared.map((r) => r.id),
    ["c", "a", "d"],
  );
  assert.deepEqual(
    mine.map((r) => r.id),
    ["b"],
  );
});

test("dc4 owners and editors edit; commenters comment; viewers read", () => {
  assert.deepEqual(
    (["owner", "editor", "commenter", "viewer"] as const).map((r) => [canEdit(r), canComment(r), roleName(r)]),
    [
      [true, true, "Owner"],
      [true, true, "Can edit"],
      [false, true, "Can comment"],
      [false, false, "Can view"],
    ],
  );
  assert.equal(canEdit(null), false);
});

test("dc4 creating names everyone in with, and commenters and viewers again", () => {
  const made = createInput({
    title: "  Q3 plan ",
    label: "confidential",
    people: [
      { login: "priya", share: "editor" },
      { login: "sam", share: "commenter" },
      { login: "lee", share: "viewer" },
      { login: "sam", share: "commenter" },
    ],
  });
  assert.deepEqual(made, { input: { title: "Q3 plan", with: ["priya", "sam", "lee"], label: "confidential", commenters: ["sam"], viewers: ["lee"] } });
  assert.deepEqual(createInput({ title: "Solo", label: null, people: [] }), { input: { title: "Solo", with: [] } });
  assert.ok("problem" in createInput({ title: " ", label: null, people: [] }));
  assert.ok("problem" in createInput({ title: "two\nlines", label: null, people: [] }));
  assert.ok("problem" in createInput({ title: "x".repeat(201), label: null, people: [] }));
  assert.ok("problem" in createInput({ title: "Secret", label: "restricted", people: [] }));
});

test("dc4 roles name editors and commenters; everyone else views, and the owner isn't named", () => {
  assert.deepEqual(
    rolesInput(ID, "me", [
      { login: "me", role: "owner" },
      { login: "priya", role: "editor" },
      { login: "sam", role: "commenter" },
      { login: "lee", role: "viewer" },
    ]),
    { doc: ID, editors: ["priya"], commenters: ["sam"] },
  );
});

test("dc3 a comment has text, and either an anchor or what it answers", () => {
  assert.deepEqual(commentInput(ID, " Totals? ", { path: "AQID" }), { input: { doc: ID, text: "Totals?", anchor: { path: "AQID" } } });
  assert.deepEqual(commentInput(ID, "Yes", { replyTo: "c1", path: "AQID" }), { input: { doc: ID, text: "Yes", reply_to: "c1" } });
  assert.deepEqual(commentInput(ID, "On the whole doc"), { input: { doc: ID, text: "On the whole doc" } });
  assert.ok("problem" in commentInput(ID, "  "));
  assert.ok("problem" in commentInput(ID, "x".repeat(8001)));
});

test("dc3 a version has a one-line name", () => {
  assert.deepEqual(versionInput(ID, " Sent to finance "), { input: { doc: ID, name: "Sent to finance" } });
  assert.ok("problem" in versionInput(ID, ""));
  assert.ok("problem" in versionInput(ID, "a\nb"));
});

test("dc3 replies sit under their comment; open anchored ones are highlighted", () => {
  const comments = [
    comment({ id: "1", anchor: { path: "AQ==" } }),
    comment({ id: "2", reply_to: "1" }),
    comment({ id: "3", anchor: { path: "Ag==" }, resolved: true }),
    comment({ id: "4", reply_to: "gone" }),
    comment({ id: "5" }),
  ];
  assert.deepEqual(
    threads(comments).map((t) => [t.comment.id, t.replies.map((r) => r.id)]),
    [
      ["1", ["2"]],
      ["3", []],
      ["4", []],
      ["5", []],
    ],
  );
  assert.deepEqual(
    anchored(comments).map((c) => c.id),
    ["1"],
  );
});

test("dc3 a comment highlights the word at its anchor", () => {
  const text = "Revenue grew 12% in Q3. Done";
  const at = (i: number) => {
    const w = wordAt(text, i);
    return w ? text.slice(w[0], w[1]) : null;
  };
  assert.equal(at(0), "Revenue");
  assert.equal(at(7), "grew");
  assert.equal(at(13), "12");
  assert.equal(at(20), "Q3");
  assert.equal(at(22), "Done");
  assert.equal(at(text.length), "Done");
  assert.deepEqual(wordAt("end.  ", 6), [0, 3]);
  assert.equal(wordAt("Sam's re-check", 0)?.[1], 5);
  assert.equal(wordAt("", 0), null);
  assert.equal(wordAt("...", 1), null);
  assert.equal(snippet("  a\n b  "), "a b");
  assert.equal(snippet("x".repeat(100), 10), `${"x".repeat(9)}…`);
});

test("dc1 links open docs, the web and mail; nothing else", () => {
  assert.equal(safeHref(`#${ID}`), `#${ID}`);
  assert.equal(safeHref("https://example.com/a?b=1"), "https://example.com/a?b=1");
  assert.equal(safeHref("mailto:priya@example.com"), "mailto:priya@example.com");
  for (const bad of ["javascript:alert(1)", "JAVASCRIPT:alert(1)", "data:text/html,x", "/etc/passwd", "#cv-1", "https://exa mple.com", "http://", "vbscript:x", " java\nscript:x", 7, null]) {
    assert.equal(safeHref(bad), null, String(bad));
  }
  assert.equal(linkFrom(ID), `#${ID}`);
  assert.equal(linkFrom(`http://docs.localhost:7300/#${ID}`), `#${ID}`);
  assert.equal(linkFrom("example.com/q3"), "https://example.com/q3");
  assert.equal(linkFrom("priya@example.com"), "mailto:priya@example.com");
  assert.equal(linkFrom("javascript:alert(1)"), null);
  assert.equal(linkFrom("not a link"), null);
});

test("dc1 an embedded artifact is found from its id or Artifacts link, and opens in Artifacts", () => {
  assert.equal(artifactFrom(ID), ID);
  assert.equal(artifactFrom(`http://artifacts.localhost:7300/#${ID}`), ID);
  assert.equal(artifactFrom(`http://${HEX}.artifact.localhost:7300/v2`), ID);
  assert.equal(artifactFrom("https://example.com/#cv-1"), null);
  assert.equal(artifactsUrl({ protocol: "http:", host: "docs.localhost:7300" }, ID), `http://artifacts.localhost:7300/#${ID}`);
  assert.equal(artifactsUrl({ protocol: "http:", host: "127.0.0.1:7300" }, ID), null);
  assert.equal(artifactsUrl({ protocol: "http:", host: "docs.localhost:7300" }, "cv-1"), null);
});

test("dc1 the slash menu narrows as you type", () => {
  assert.equal(slashMatches("").length, 16);
  assert.deepEqual(
    slashMatches("head").map((i) => i.kind),
    ["h1", "h2", "h3"],
  );
  assert.deepEqual(
    slashMatches("todo").map((i) => i.kind),
    ["task"],
  );
  assert.deepEqual(
    slashMatches("tab").map((i) => i.kind),
    ["table"],
  );
  assert.deepEqual(slashMatches("zzz"), []);
});

test("br1 the base system is never named on screen", () => {
  assert.equal(readable("FreeBSD jail refused it"), "the system jail refused it");
  assert.equal(readable("free bsd"), "the system");
});
