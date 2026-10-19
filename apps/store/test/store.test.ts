// The Store's logic (DV-10, DV-11): yours and available, search, who made
// it, what it may do (DV-3), and the empty pages.

import assert from "node:assert/strict";
import { test } from "node:test";

import { type Item, canDo, emptyText, publisherLine, sections } from "../src/store.ts";

const item = (id: string, extra: Partial<Item> = {}): Item => ({
  id,
  kind: "app",
  name: id,
  version: "1.0.0",
  publisher: "Acme IT",
  publisher_verified: false,
  internal: false,
  install: "available",
  installed: false,
  summary: `The ${id}`,
  declares: { actions: ["records.read"], max_label: "internal", egress: [] },
  ...extra,
});

test("dv11 required and installed items are yours; the rest you may install", () => {
  const { yours, available } = sections([
    item("vendor-disputes"),
    item("expense-check", { install: "required", installed: true }),
    item("contract-review", { installed: true }),
  ]);
  assert.deepEqual(yours.map((i) => i.id), ["contract-review", "expense-check"]);
  assert.deepEqual(available.map((i) => i.id), ["vendor-disputes"]);
});

test("search matches every word in the name, summary, publisher or kind", () => {
  const items = [item("vendor-disputes", { summary: "Overdue reminders" }), item("payroll", { kind: "agent" })];
  assert.deepEqual(sections(items, "overdue VENDOR").available.map((i) => i.id), ["vendor-disputes"]);
  assert.deepEqual(sections(items, "agent").available.map((i) => i.id), ["payroll"]);
  assert.deepEqual(sections(items, "nothing").available, []);
});

test("dv3 what an item may do reads in one line", () => {
  assert.equal(canDo(item("x").declares), "records.read · data up to internal · no network of its own");
  assert.equal(
    canDo({ actions: [], max_label: "public", egress: ["api.example.com"] }),
    "nothing on its own · data up to public · reaches api.example.com",
  );
  assert.equal(publisherLine(item("x", { internal: true })), "Acme IT · built by your organization");
  assert.equal(publisherLine(item("x", { publisher_verified: true })), "Acme IT · verified publisher");
});

test("dv13 the empty pages say why", () => {
  assert.match(emptyText({ catalog_version: null, managed: false, items: [] }) ?? "", /isn't managed/);
  assert.match(emptyText({ catalog_version: null, managed: true, items: [] }) ?? "", /No catalog from IT/);
  assert.match(emptyText({ catalog_version: 3, managed: true, items: [] }) ?? "", /offers you nothing yet/);
  assert.equal(emptyText({ catalog_version: 3, managed: true, items: [item("x")] }), null);
});
