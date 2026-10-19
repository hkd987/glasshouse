// Addressing people (AD-1): the address being typed, suggestions from the
// directory and recent mail, putting one in, and moving through them.

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  addressesIn,
  currentToken,
  insertAddress,
  matches,
  mergeSuggestions,
  narrowed,
  type Person,
  recentPeople,
  stepHighlight,
} from "../src/people.ts";

const priya: Person = { name: "Priya Shah", email: "priya@acme.example", title: "Counsel" };
const dana: Person = { name: "Dana Ruiz", email: "dana@acme.example" };
const vendor: Person = { name: "Pria Vendor", email: "pria@northwind.example" };

test("ad1 the address being typed is the one at the caret", () => {
  assert.deepEqual(currentToken("dana@acme.example, pri", 22), { start: 19, end: 22, token: "pri" });
  assert.deepEqual(currentToken("pri, dana@acme.example", 2), { start: 0, end: 3, token: "pri" });
  assert.deepEqual(currentToken("", 0), { start: 0, end: 0, token: "" });
  assert.deepEqual(currentToken("a@b.c, ", 7), { start: 7, end: 7, token: "" });
});

test("ad1 picking a suggestion puts its address in place of what was typed", () => {
  assert.deepEqual(insertAddress("dana@acme.example, pri", 19, 22, "priya@acme.example"), {
    text: "dana@acme.example, priya@acme.example, ",
    caret: 39,
  });
  // In the middle: what follows stays, after one separator.
  assert.deepEqual(insertAddress("pri, dana@acme.example", 0, 3, "priya@acme.example").text, "priya@acme.example, dana@acme.example");
  assert.deepEqual(addressesIn("Priya@acme.example, x; dana@acme.example"), ["priya@acme.example", "dana@acme.example"]);
});

test("ad1 suggestions put the directory first, then recent mail, never twice or anyone already in", () => {
  assert.ok(matches(priya, "pri") && matches(priya, "shah") && matches(priya, "priya@"));
  assert.ok(!matches(dana, "pri") && !matches(priya, ""));
  const shown = mergeSuggestions([priya], [vendor, { ...priya, name: "Priya" }, dana], [], "pri");
  assert.deepEqual(
    shown.map((p) => p.email),
    ["priya@acme.example", "pria@northwind.example"],
  );
  assert.deepEqual(mergeSuggestions([priya], [vendor], ["PRIYA@acme.example"], "pri"), [vendor]);
  assert.equal(mergeSuggestions(Array(20).fill(0).map((_, n) => ({ name: `P ${n}`, email: `p${n}@a.b` })), [], [], "p", 8).length, 8);
});

test("ad1 a complete earlier result narrows locally without another lookup", () => {
  const cache = new Map<string, Person[]>([["pr", [priya, vendor]]]);
  assert.deepEqual(narrowed(cache, "priy"), [priya]);
  // An earlier result that hit the limit may have left people out: ask again.
  const full = new Map<string, Person[]>([["pr", Array(8).fill(priya)]]);
  assert.equal(narrowed(full, "priy"), null);
  assert.equal(narrowed(new Map(), "priy"), null);
});

test("ui5 arrows move through the suggestions and wrap round", () => {
  assert.equal(stepHighlight(0, 3, "ArrowDown"), 1);
  assert.equal(stepHighlight(2, 3, "ArrowDown"), 0);
  assert.equal(stepHighlight(0, 3, "ArrowUp"), 2);
  assert.equal(stepHighlight(-1, 0, "ArrowDown"), -1);
});

test("ad1 recent mail gives names and addresses once each", () => {
  assert.deepEqual(recentPeople(["Dana Ruiz <dana@acme.example>", "dana@acme.example", "demo", "\"Shah, Priya\" <priya@acme.example>"]), [
    { name: "Dana Ruiz", email: "dana@acme.example" },
    { name: "Shah, Priya", email: "priya@acme.example" },
  ]);
});
