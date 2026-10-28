// An agent computer's screen, read a piece at a time from an offset in
// bytes (ADR 0065 §4, ADR 0066): appended in order, overlaps and gaps
// handled, terminal control turned into plain text.

import assert from "node:assert/strict";
import { test } from "node:test";

import { ScreenBuffer, plain } from "../src/screen.ts";
import { fixture } from "./fixture.ts";

const bytes = (text: string) => new TextEncoder().encode(text).length;

/** A screen that grows, read as glass-agents answers `?since=`. */
function screen(text: string) {
  return {
    since(offset: number) {
      return { offset: bytes(text), text: new TextDecoder().decode(new TextEncoder().encode(text).subarray(offset)) };
    },
    add(more: string) {
      text += more;
    },
  };
}

test("adr0066 the screen appends from where the last read ended", () => {
  const s = screen("$ ls\nin out\n");
  const b = new ScreenBuffer();
  assert.equal(b.apply(s.since(b.offset)), "appended");
  assert.equal(b.text, "$ ls\nin out\n");
  assert.equal(b.apply(s.since(b.offset)), "nothing");
  s.add("$ pwd\n/home/agent\n");
  assert.equal(b.apply(s.since(b.offset)), "appended");
  assert.equal(b.text, "$ ls\nin out\n$ pwd\n/home/agent\n");
  assert.equal(b.offset, bytes(b.text));
});

test("adr0066 offsets are bytes: text past ASCII lands where it should", () => {
  const s = screen("Matching ████ 214/214\n");
  const b = new ScreenBuffer();
  b.apply(s.since(0));
  s.add("Total: 3 615,50 €\n");
  b.apply(s.since(b.offset));
  assert.equal(b.text, "Matching ████ 214/214\nTotal: 3 615,50 €\n");
});

test("adr0066 typed input's answer, the whole screen from its start, adds only what's new", () => {
  // glass-agents answers POST …/input with the screen from 0.
  const s = screen("$ ");
  const b = new ScreenBuffer();
  b.apply(s.since(0));
  s.add("ls\nin out\n$ ");
  assert.equal(b.apply(s.since(0)), "appended");
  assert.equal(b.text, "$ ls\nin out\n$ ");
  // The same again changes nothing.
  assert.equal(b.apply(s.since(0)), "nothing");
});

test("adr0066 skipped output is marked, and a screen that started over replaces it", () => {
  const b = new ScreenBuffer();
  b.apply({ offset: 6, text: "hello\n" });
  // The front kept only the last bytes: something in between is missing.
  assert.equal(b.apply({ offset: 100, text: "later\n" }), "gap");
  assert.equal(b.text, "hello\n…\nlater\n");
  assert.equal(b.offset, 100);
  // Its computer was started over: the screen is shorter than before.
  assert.equal(b.apply({ offset: 9, text: "fresh\n$ " }), "reset");
  assert.equal(b.text, "fresh\n$ ");
  assert.equal(b.offset, 9);
});

test("adr0066 colors and cursor moves come out, even split across reads", () => {
  assert.deepEqual(plain("\x1b[32magent@reconciler\x1b[0m:\x1b[34m~\x1b[0m$ ls"), { text: "agent@reconciler:~$ ls", carry: "" });
  assert.deepEqual(plain("\x1b]0;a title\x07done"), { text: "done", carry: "" });
  assert.deepEqual(plain("red: \x1b[3"), { text: "red: ", carry: "\x1b[3" });
  const b = new ScreenBuffer();
  b.apply({ offset: bytes("red: \x1b[3"), text: "red: \x1b[3" });
  b.apply({ offset: bytes("red: \x1b[31mstop\x1b[0m"), text: "1mstop\x1b[0m" });
  assert.equal(b.text, "red: stop");
  // A bell or other control character doesn't show.
  assert.equal(plain("ding\x07 \x00done").text, "ding done");
});

test("adr0066 a carriage return writes over its line, as a progress bar does", () => {
  const b = new ScreenBuffer();
  b.apply({ offset: 12, text: "Copying 10%\r" });
  b.apply({ offset: 24, text: "Copying 90%\r" });
  b.apply({ offset: 38, text: "Copying done\r\n" });
  assert.equal(b.text, "Copying done\n");
  // \r\n split across two reads is still one line end.
  const c = new ScreenBuffer();
  c.apply({ offset: 4, text: "one\r" });
  c.apply({ offset: 9, text: "\ntwo\n" });
  assert.equal(c.text, "one\ntwo\n");
  // Backspace takes back a letter, not a line.
  const d = new ScreenBuffer();
  d.apply({ offset: bytes("lss\x08 \x08\n"), text: "lss\x08 \x08\n" });
  assert.equal(d.text, "ls\n");
});

test("adr0066 a long-running screen keeps its last part, from a line's start", () => {
  const b = new ScreenBuffer(40);
  let text = "";
  for (let i = 0; i < 20; i++) text += `line ${i}\n`;
  b.apply({ offset: bytes(text), text });
  assert.ok(b.text.length <= 40);
  assert.ok(b.text.startsWith("line "));
  assert.ok(b.text.endsWith("line 19\n"));
});

test("adr0066 the fixture's screens read as plain text", () => {
  const b = new ScreenBuffer();
  const text = fixture().screens.reconciler ?? "";
  b.apply({ offset: bytes(text), text });
  assert.doesNotMatch(b.text, /\x1b|\r/);
  assert.match(b.text, /^agent@reconciler:~\$ ls in\//);
  assert.match(b.text, /Asking Dana before sending 3 emails/);
});
