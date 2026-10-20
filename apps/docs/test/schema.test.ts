// The document's schema is a contract with the computer (ADR 0035 §1): yrs
// reads and writes the same Yjs document to give agents Markdown, so the
// names y-prosemirror writes must be exactly the ones glass-app-docs reads.
// This writes a doc with every node and mark into a Yjs fragment, checks
// what lands there, and reads it back. It also covers the editor's own
// commands and input rules on a state, without a page.

import assert from "node:assert/strict";
import { test } from "node:test";

import type { Node as PMNode } from "prosemirror-model";
import { EditorState, type Plugin, TextSelection, type Transaction } from "prosemirror-state";
import { prosemirrorToYXmlFragment, yXmlFragmentToProseMirrorRootNode } from "y-prosemirror";
import * as Y from "yjs";

import { blockCommand, markdownRules, table } from "../src/editor.ts";
import { FRAGMENT } from "../src/live.ts";
import { level, MARK_NAMES, NODE_NAMES, schema, tone } from "../src/schema.ts";

const HEX = "0123456789abcdef0123456789abcdef";

const text = (t: string, marks: { type: string; attrs?: Record<string, unknown> }[] = []) => ({ type: "text", text: t, marks });
const p = (...content: unknown[]) => ({ type: "paragraph", content });

/** One of everything. */
const everything = {
  type: "doc",
  content: [
    { type: "heading", attrs: { level: 2 }, content: [text("Q3 plan")] },
    p(
      text("Plain, "),
      text("bold", [{ type: "strong" }]),
      text(", "),
      text("em", [{ type: "em" }]),
      text(", "),
      text("code", [{ type: "code" }]),
      text(", "),
      text("struck", [{ type: "strike" }]),
      text(", "),
      text("a link", [{ type: "link", attrs: { href: `#cv-${HEX}` } }]),
      { type: "hard_break" },
      text("written by an agent", [{ type: "ai", attrs: { by: "claude-code" } }]),
    ),
    { type: "blockquote", content: [p(text("Quoted"))] },
    { type: "bullet_list", content: [{ type: "list_item", content: [p(text("One"))] }] },
    { type: "ordered_list", attrs: { order: 3 }, content: [{ type: "list_item", content: [p(text("Three"))] }] },
    {
      type: "task_list",
      content: [
        { type: "task_item", attrs: { checked: true }, content: [p(text("Done"))] },
        { type: "task_item", attrs: { checked: false }, content: [p(text("Not yet"))] },
      ],
    },
    { type: "code_block", attrs: { language: "rust" }, content: [text("fn main() {}")] },
    { type: "callout", attrs: { tone: "warn" }, content: [p(text("Careful"))] },
    { type: "horizontal_rule" },
    {
      type: "table",
      content: [
        { type: "table_row", content: [{ type: "table_header", content: [p(text("Line"))] }, { type: "table_header", content: [p(text("Q3"))] }] },
        { type: "table_row", content: [{ type: "table_cell", content: [p(text("Licenses"))] }, { type: "table_cell", content: [p(text("412"))] }] },
      ],
    },
    { type: "artifact", attrs: { id: `cv-${HEX}`, title: "Q3 forecast" } },
  ],
};

interface Seen {
  names: Set<string>;
  attrs: Map<string, Record<string, unknown>[]>;
  marks: Map<string, unknown[]>;
}

function walk(el: Y.XmlElement | Y.XmlFragment, seen: Seen): void {
  for (const child of el.toArray()) {
    if (child instanceof Y.XmlText) {
      seen.names.add("text");
      for (const op of child.toDelta() as { insert: unknown; attributes?: Record<string, unknown> }[]) {
        for (const [name, value] of Object.entries(op.attributes ?? {})) seen.marks.set(name, [...(seen.marks.get(name) ?? []), JSON.parse(JSON.stringify(value))]);
      }
    } else if (child instanceof Y.XmlElement) {
      seen.names.add(child.nodeName);
      seen.attrs.set(child.nodeName, [...(seen.attrs.get(child.nodeName) ?? []), child.getAttributes() as Record<string, unknown>]);
      walk(child, seen);
    }
  }
}

test("dc1 the schema names exactly the nodes and marks the computer knows", () => {
  assert.deepEqual(Object.keys(schema.nodes).sort(), [...NODE_NAMES].sort());
  assert.deepEqual(Object.keys(schema.marks).sort(), [...MARK_NAMES].sort());
});

test("dc6 a doc's Yjs form is what yrs reads: element names, attributes and marks", () => {
  const node = schema.nodeFromJSON(everything);
  node.check();
  const ydoc = new Y.Doc();
  prosemirrorToYXmlFragment(node, ydoc.getXmlFragment(FRAGMENT));
  const seen: Seen = { names: new Set(), attrs: new Map(), marks: new Map() };
  walk(ydoc.getXmlFragment(FRAGMENT), seen);

  // Every node but `doc` (the fragment itself) and `hard_break` (an element too) is there by name.
  assert.deepEqual([...seen.names].sort(), NODE_NAMES.filter((name) => name !== "doc").sort());
  assert.deepEqual(seen.attrs.get("heading"), [{ level: 2 }]);
  assert.deepEqual(seen.attrs.get("ordered_list"), [{ order: 3 }]);
  assert.deepEqual(seen.attrs.get("task_item"), [{ checked: true }, { checked: false }]);
  assert.deepEqual(seen.attrs.get("code_block"), [{ language: "rust" }]);
  assert.deepEqual(seen.attrs.get("callout"), [{ tone: "warn" }]);
  assert.deepEqual(seen.attrs.get("artifact"), [{ id: `cv-${HEX}`, title: "Q3 forecast" }]);
  // prosemirror-tables' cells carry their spans (colwidth is null, so not written).
  assert.deepEqual(seen.attrs.get("table_cell"), [
    { colspan: 1, rowspan: 1 },
    { colspan: 1, rowspan: 1 },
  ]);
  for (const plain of ["paragraph", "blockquote", "bullet_list", "list_item", "task_list", "horizontal_rule", "table", "table_row", "hard_break"]) {
    for (const attrs of seen.attrs.get(plain) ?? []) assert.deepEqual(attrs, {}, plain);
  }
  // Marks are text attributes named after the mark, each the mark's attrs.
  assert.deepEqual([...seen.marks.keys()].sort(), [...MARK_NAMES].sort());
  assert.deepEqual(seen.marks.get("strong"), [{}]);
  assert.deepEqual(seen.marks.get("link"), [{ href: `#cv-${HEX}` }]);
  assert.deepEqual(seen.marks.get("ai"), [{ by: "claude-code" }]);

  // Another computer reads the same doc back from the update.
  const other = new Y.Doc();
  Y.applyUpdate(other, Y.encodeStateAsUpdate(ydoc));
  const back = yXmlFragmentToProseMirrorRootNode(other.getXmlFragment(FRAGMENT), schema);
  assert.ok(back.eq(node), "the doc reads back the same");
});

test("dc6 what an agent writes with only the names (no spans, odd values) still reads", () => {
  // As yrs might write it: a table without colspan, a heading level as a string.
  const ydoc = new Y.Doc();
  const fragment = ydoc.getXmlFragment(FRAGMENT);
  const heading = new Y.XmlElement("heading");
  const tableEl = new Y.XmlElement("table");
  fragment.insert(0, [heading, tableEl]);
  heading.setAttribute("level", "2" as never);
  heading.insert(0, [new Y.XmlText("From an agent")]);
  const row = new Y.XmlElement("table_row");
  tableEl.insert(0, [row]);
  const cell = new Y.XmlElement("table_cell");
  row.insert(0, [cell]);
  const para = new Y.XmlElement("paragraph");
  cell.insert(0, [para]);
  const t = new Y.XmlText();
  para.insert(0, [t]);
  t.insert(0, "412", { ai: { by: "claude-code" } });
  const node = yXmlFragmentToProseMirrorRootNode(fragment, schema);
  assert.equal(node.childCount, 2);
  assert.equal(level(node.child(0).attrs.level), 2);
  assert.equal(node.child(1).firstChild?.firstChild?.attrs.colspan, 1);
  assert.equal(node.child(1).textContent, "412");
  assert.equal(tone("loud"), "info");
});

// --- The editor's commands and input rules, on a state -----------------------

/** A stand-in for the view the input rules plugin asks for: a state and dispatch. */
function fakeView(doc: unknown, at: number) {
  const plugins: Plugin[] = [markdownRules()];
  let state = EditorState.create({ doc: schema.nodeFromJSON(doc), plugins });
  state = state.apply(state.tr.setSelection(TextSelection.create(state.doc, at)));
  const view = {
    get state() {
      return state;
    },
    composing: false,
    dispatch(tr: Transaction) {
      state = state.apply(tr);
    },
  };
  /** Types `chars` one at a time, as the browser would, through the rules. */
  const type = (chars: string) => {
    for (const ch of chars) {
      const { from, to } = state.selection;
      const handled = plugins.some((plugin) => plugin.props.handleTextInput?.call(plugin, view as never, from, to, ch, () => state.tr.insertText(ch, from, to)));
      if (!handled) view.dispatch(state.tr.insertText(ch, from, to));
    }
  };
  return { view, type, doc: () => state.doc as PMNode };
}

const empty = { type: "doc", content: [{ type: "paragraph" }] };

test("dc1 markdown-ish rules make blocks as you type", () => {
  const cases: [string, (doc: PMNode) => void][] = [
    ["# ", (d) => assert.deepEqual([d.firstChild?.type.name, d.firstChild?.attrs.level], ["heading", 1])],
    ["### ", (d) => assert.deepEqual([d.firstChild?.type.name, d.firstChild?.attrs.level], ["heading", 3])],
    ["- ", (d) => assert.equal(d.firstChild?.type.name, "bullet_list")],
    ["1. ", (d) => assert.deepEqual([d.firstChild?.type.name, d.firstChild?.attrs.order], ["ordered_list", 1])],
    ["[ ] ", (d) => assert.deepEqual([d.firstChild?.type.name, d.firstChild?.firstChild?.attrs.checked], ["task_list", false])],
    ["[x] ", (d) => assert.deepEqual([d.firstChild?.type.name, d.firstChild?.firstChild?.attrs.checked], ["task_list", true])],
    ["- [ ] ", (d) => assert.deepEqual([d.firstChild?.type.name, d.firstChild?.firstChild?.type.name], ["task_list", "task_item"])],
    ["```ts ", (d) => assert.deepEqual([d.firstChild?.type.name, d.firstChild?.attrs.language], ["code_block", "ts"])],
    ["> ", (d) => assert.equal(d.firstChild?.type.name, "blockquote")],
    ["---", (d) => assert.deepEqual([d.child(0).type.name, d.child(1).type.name], ["horizontal_rule", "paragraph"])],
  ];
  for (const [typed, check] of cases) {
    const v = fakeView(empty, 1);
    v.type(typed);
    check(v.doc());
    v.doc().check();
  }
});

test("dc1 **bold**, *em*, `code` and ~~strike~~ become marks without their delimiters", () => {
  for (const [typed, mark, word] of [
    ["say **loud** ", "strong", "loud"],
    ["say *soft* ", "em", "soft"],
    ["say _soft_ ", "em", "soft"],
    ["run `make` ", "code", "make"],
    ["was ~~wrong~~ ", "strike", "wrong"],
  ] as const) {
    const v = fakeView(empty, 1);
    v.type(typed);
    const para = v.doc().firstChild;
    assert.equal(para?.textContent, typed.replace(/[*_`~]/g, ""), typed);
    let marked = "";
    para?.forEach((child) => {
      if (child.marks.some((m) => m.type.name === mark)) marked += child.text ?? "";
    });
    assert.equal(marked, word, typed);
  }
});

test("dc1 slash items change the block or insert one, and a table has a header row", () => {
  const run = (kind: Parameters<typeof blockCommand>[0], doc: unknown = { type: "doc", content: [p(text("Hello"))] }) => {
    let state = EditorState.create({ doc: schema.nodeFromJSON(doc) });
    state = state.apply(state.tr.setSelection(TextSelection.create(state.doc, Math.min(3, state.doc.content.size - 1))));
    const command = blockCommand(kind);
    assert.ok(command, kind);
    assert.ok(command(state, (tr) => (state = state.apply(tr))), kind);
    state.doc.check();
    return state.doc;
  };
  assert.equal(run("h2").firstChild?.attrs.level, 2);
  assert.equal(run("task").firstChild?.type.name, "task_list");
  assert.equal(run("code").firstChild?.type.name, "code_block");
  assert.deepEqual([run("success").firstChild?.type.name, run("success").firstChild?.attrs.tone], ["callout", "success"]);
  const withTable = run("table");
  assert.deepEqual(
    Array.from({ length: withTable.childCount }, (_, i) => withTable.child(i).type.name),
    ["paragraph", "table", "paragraph"],
  );
  const t = table(3, 3);
  assert.equal(t.childCount, 3);
  assert.equal(t.firstChild?.firstChild?.type.name, "table_header");
  assert.equal(t.child(1).firstChild?.type.name, "table_cell");
  // In an empty line the block takes its place.
  const inEmpty = run("rule", empty);
  assert.deepEqual(
    Array.from({ length: inEmpty.childCount }, (_, i) => inEmpty.child(i).type.name),
    ["horizontal_rule", "paragraph"],
  );
});
