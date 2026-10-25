// The Store's logic (DV-10, DV-11): yours and available, search, who made
// it, what it may do (DV-3), and the empty pages.

import assert from "node:assert/strict";
import { test } from "node:test";

import { type Item, canDo, emptyText, patchState, publisherLine, sections } from "../src/store.ts";

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

test("ga13 an app someone at the org made says who, and what it remixes", () => {
  const listing = [item("mail-with-write", { publisher: "Dana Ruiz", internal: true, app: { made_by: "dana", based_on: "mail@0.1.0" } })];
  assert.equal(publisherLine(listing[0]!, listing), "Made by Dana Ruiz · remix of mail");
  // The listing's name for what it remixes, when the catalog has it.
  const both = [...listing, item("mail", { name: "Mail" })];
  assert.equal(publisherLine(listing[0]!, both), "Made by Dana Ruiz · remix of Mail");
  assert.equal(
    publisherLine(item("expenses", { publisher: "", app: { made_by: "sam", based_on: null } })),
    "Made by sam · a new app",
  );
  // What nobody at the org made reads as before.
  assert.equal(publisherLine(item("x", { internal: true, app: null })), "Acme IT · built by your organization");
});

test("dv13 the empty pages say why", () => {
  assert.match(emptyText({ catalog_version: null, managed: false, items: [] }) ?? "", /isn't managed/);
  assert.match(emptyText({ catalog_version: null, managed: true, items: [] }) ?? "", /No catalog from IT/);
  assert.match(emptyText({ catalog_version: 3, managed: true, items: [] }) ?? "", /offers you nothing yet/);
  assert.equal(emptyText({ catalog_version: 3, managed: true, items: [item("x")] }), null);
});

test("adr0057 kernel patches for this computer are in Hardware, not with apps", () => {
  const patch = { patch: "iwlwifi-resume", targets: ["pci:8086:2725"], kernel: false, bytes: 4096, reviewed_by: "pat", state: null };
  const { yours, available, hardware } = sections([
    item("vendor-disputes"),
    item("iwlwifi-resume", { kind: "hardware_patch", name: "Wi-Fi works again after sleep", patch }),
  ]);
  assert.deepEqual(available.map((i) => i.id), ["vendor-disputes"]);
  assert.deepEqual(yours, []);
  assert.deepEqual(hardware.map((i) => i.id), ["iwlwifi-resume"]);
  assert.equal(patchState(patch), null);
  assert.equal(patchState({ ...patch, state: "staged" }), "Restart to try it");
  assert.equal(patchState({ ...patch, state: "rolled_back" }), "It didn't work here and went back");
});
