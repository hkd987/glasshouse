// The block editor (DC-1): ProseMirror bound to the doc's Yjs document by
// y-prosemirror, so every edit is a Yjs update (live.ts sends them) and
// undo is Yjs's, which never undoes anyone else's edit.
//
// What it adds to ProseMirror: the slash menu ("/"), Markdown-ish input
// rules (#, -, 1., [ ], ```, >, ---, **bold**), a bubble over a selection
// for marks, links and comments, tools for the table the cursor is in,
// checklists, callouts and artifact cards (a card that opens Artifacts,
// never a frame), and comment highlights at their anchors. Everything it
// draws is made with h(), so text from a doc is only ever text.

import { h, show } from "@glasshouse/sdk";
import { baseKeymap, chainCommands, exitCode, setBlockType, toggleMark, wrapIn } from "prosemirror-commands";
import { dropCursor } from "prosemirror-dropcursor";
import { gapCursor } from "prosemirror-gapcursor";
import { InputRule, inputRules, textblockTypeInputRule, undoInputRule, wrappingInputRule } from "prosemirror-inputrules";
import { keymap } from "prosemirror-keymap";
import type { MarkType, Node as PMNode } from "prosemirror-model";
import { liftListItem, sinkListItem, splitListItem, wrapInList } from "prosemirror-schema-list";
import { type Command, EditorState, Plugin, PluginKey, TextSelection } from "prosemirror-state";
import { addColumnAfter, addRowAfter, CellSelection, deleteColumn, deleteRow, deleteTable, goToNextCell, isInTable, tableEditing, toggleHeaderRow } from "prosemirror-tables";
import { canJoin, findWrapping } from "prosemirror-transform";
import { Decoration, DecorationSet, EditorView, type NodeView, type ViewMutationRecord } from "prosemirror-view";
import { absolutePositionToRelativePosition, redoCommand, relativePositionToAbsolutePosition, undoCommand, ySyncPlugin, ySyncPluginKey, yUndoPlugin } from "y-prosemirror";
import type * as Y from "yjs";

import { anchored, artifactsUrl, type BlockKind, type Comment, deepLink, linkFrom, safeHref, type SlashItem, slashMatches, snippet, wordAt } from "./docs.ts";
import { decodeAnchor, encodeAnchor, FRAGMENT } from "./live.ts";
import { language, schema, type Tone, TONES, tone } from "./schema.ts";

/** What the editor asks of the page. */
export interface Hooks {
  /** Whether the person may edit now (their role, and the system reachable). */
  editable(): boolean;
  /** Whether they may comment (owners, editors and commenters). */
  canComment(): boolean;
  /** "Comment" on a selection: where it goes and what it quotes. */
  comment(at: { path: string; quote: string }): void;
  /** A comment's highlight was clicked. */
  focusComment(id: string): void;
  /** A link to another doc was followed. */
  openDoc(id: string): void;
  /** A doc's title, for a link to it. */
  titleOf(id: string): string | null;
  /** The slash menu's "Link to a doc": which one. */
  pickDoc(): Promise<{ id: string; title: string } | null>;
  /** The slash menu's "Artifact": which one, and its title. */
  pickArtifact(): Promise<{ id: string; title: string } | null>;
}

const n = schema.nodes;
const m = schema.marks;

/** Whatever node type a name is, it is in the schema (schema.ts is fixed). */
function must<T>(value: T | null | undefined, name: string): T {
  if (value === undefined || value === null) throw new Error(`the schema has no ${name}`);
  return value;
}

const NODES = {
  paragraph: must(n.paragraph, "paragraph"),
  heading: must(n.heading, "heading"),
  blockquote: must(n.blockquote, "blockquote"),
  bullet_list: must(n.bullet_list, "bullet_list"),
  ordered_list: must(n.ordered_list, "ordered_list"),
  list_item: must(n.list_item, "list_item"),
  task_list: must(n.task_list, "task_list"),
  task_item: must(n.task_item, "task_item"),
  code_block: must(n.code_block, "code_block"),
  callout: must(n.callout, "callout"),
  horizontal_rule: must(n.horizontal_rule, "horizontal_rule"),
  table: must(n.table, "table"),
  table_row: must(n.table_row, "table_row"),
  table_cell: must(n.table_cell, "table_cell"),
  table_header: must(n.table_header, "table_header"),
  artifact: must(n.artifact, "artifact"),
  hard_break: must(n.hard_break, "hard_break"),
};
const MARKS = {
  strong: must(m.strong, "strong"),
  em: must(m.em, "em"),
  strike: must(m.strike, "strike"),
  code: must(m.code, "code"),
  link: must(m.link, "link"),
};

// --- Commands ----------------------------------------------------------

/** Puts `node` where the cursor's block is when that block is empty, else after it; a paragraph follows. */
function insertBlock(node: PMNode): Command {
  return (state, dispatch) => {
    const { $from } = state.selection;
    if (!$from.parent.isTextblock || $from.depth < 1) return false;
    const d = $from.depth;
    const parent = $from.node(d - 1);
    const index = $from.index(d - 1);
    const tr = state.tr;
    let at: number;
    if ($from.parent.content.size === 0 && parent.canReplaceWith(index, index + 1, node.type)) {
      at = $from.before(d);
      tr.replaceWith(at, $from.after(d), node);
    } else if (parent.canReplaceWith(index + 1, index + 1, node.type)) {
      at = $from.after(d);
      tr.insert(at, node);
    } else {
      return false;
    }
    const end = at + node.nodeSize;
    const next = tr.doc.resolve(end).nodeAfter;
    if (!next || !next.isTextblock) tr.insert(end, NODES.paragraph.create());
    const inside = node.type === NODES.table ? at + 4 : end + 1;
    tr.setSelection(TextSelection.near(tr.doc.resolve(Math.min(inside, tr.doc.content.size))));
    dispatch?.(tr.scrollIntoView());
    return true;
  };
}

/** A table with a header row, `rows` by `cols`. */
export function table(rows: number, cols: number): PMNode {
  const cell = (type: typeof NODES.table_cell) => must(type.createAndFill(), "an empty cell");
  const row = (type: typeof NODES.table_cell) => NODES.table_row.create(null, Array.from({ length: cols }, () => cell(type)));
  return NODES.table.create(null, [row(NODES.table_header), ...Array.from({ length: rows - 1 }, () => row(NODES.table_cell))]);
}

/** Where the callout around the cursor starts, if it is in one. */
function calloutAt(state: EditorState): { pos: number; node: PMNode } | null {
  const { $from } = state.selection;
  for (let d = $from.depth; d > 0; d--) {
    const node = $from.node(d);
    if (node.type === NODES.callout) return { pos: $from.before(d), node };
  }
  return null;
}

function callout(t: Tone): Command {
  return (state, dispatch) => {
    const found = calloutAt(state);
    if (found) {
      dispatch?.(state.tr.setNodeMarkup(found.pos, undefined, { ...found.node.attrs, tone: t }));
      return true;
    }
    return wrapIn(NODES.callout, { tone: t })(state, dispatch);
  };
}

/** What a slash item does to the cursor's block (the async ones are the editor's). */
export function blockCommand(kind: BlockKind): Command | null {
  switch (kind) {
    case "paragraph":
      return setBlockType(NODES.paragraph);
    case "h1":
    case "h2":
    case "h3":
      return setBlockType(NODES.heading, { level: Number(kind.slice(1)) });
    case "bullet":
      return wrapInList(NODES.bullet_list);
    case "ordered":
      return wrapInList(NODES.ordered_list);
    case "task":
      return wrapInList(NODES.task_list);
    case "quote":
      return wrapIn(NODES.blockquote);
    case "code":
      return setBlockType(NODES.code_block, { language: "" });
    case "info":
    case "warn":
    case "success":
      return callout(kind);
    case "rule":
      return insertBlock(NODES.horizontal_rule.create());
    case "table":
      return insertBlock(table(3, 3));
    default:
      return null;
  }
}

/** Backspace at the start of a heading or code block makes it text again (as Notion does). */
const plainAtStart: Command = (state, dispatch) => {
  const sel = state.selection;
  if (!(sel instanceof TextSelection) || !sel.$cursor || sel.$cursor.parentOffset > 0) return false;
  const block = sel.$cursor.parent;
  if (block.type === NODES.paragraph || !block.isTextblock) return false;
  return setBlockType(NODES.paragraph)(state, dispatch);
};

/** Backspace at the start of a list item's first line takes it out of the list. */
const liftAtStart: Command = (state, dispatch) => {
  const sel = state.selection;
  if (!(sel instanceof TextSelection) || !sel.$cursor || sel.$cursor.parentOffset > 0) return false;
  const $c = sel.$cursor;
  if ($c.depth < 2 || $c.index($c.depth - 1) !== 0) return false;
  const item = $c.node($c.depth - 1);
  if (item.type === NODES.list_item) return liftListItem(NODES.list_item)(state, dispatch);
  if (item.type === NODES.task_item) return liftListItem(NODES.task_item)(state, dispatch);
  return false;
};

/** Mod-Enter in a checklist ticks the item the cursor is in. */
const toggleTask: Command = (state, dispatch) => {
  const { $from } = state.selection;
  for (let d = $from.depth; d > 0; d--) {
    const node = $from.node(d);
    if (node.type === NODES.task_item) {
      dispatch?.(state.tr.setNodeMarkup($from.before(d), undefined, { ...node.attrs, checked: node.attrs.checked !== true }));
      return true;
    }
  }
  return false;
};

const hardBreak: Command = (state, dispatch) => {
  dispatch?.(state.tr.replaceSelectionWith(NODES.hard_break.create()).scrollIntoView());
  return true;
};

function markActive(state: EditorState, type: MarkType): boolean {
  const { from, $from, to, empty } = state.selection;
  if (empty) return Boolean(type.isInSet(state.storedMarks ?? $from.marks()));
  return state.doc.rangeHasMark(from, to, type);
}

/** The link mark around the cursor: its range and href. */
function linkAround(state: EditorState): { from: number; to: number; href: string } | null {
  const { $from } = state.selection;
  const mark = MARKS.link.isInSet($from.marks()) ?? ($from.nodeBefore ? MARKS.link.isInSet($from.nodeBefore.marks) : undefined);
  if (!mark) return null;
  const parent = $from.parent;
  const start = $from.start();
  let from = -1;
  let to = -1;
  parent.forEach((child, offset) => {
    const at = start + offset;
    const has = mark.isInSet(child.marks);
    if (has && at <= $from.pos && at + child.nodeSize >= $from.pos && from < 0) {
      from = at;
      to = at + child.nodeSize;
    } else if (has && at === to) {
      to = at + child.nodeSize;
    }
  });
  if (from < 0) return null;
  return { from, to, href: String(mark.attrs.href) };
}

// --- Input rules ---------------------------------------------------------

/** `**bold**`, `*em*`, `` `code` ``, `~~strike~~`: group 1 is what precedes the opening, group 2 the text. */
function markRule(regexp: RegExp, type: MarkType, delimiter: number): InputRule {
  return new InputRule(regexp, (state, match, start, end) => {
    const before = match[1] ?? "";
    const text = match[2];
    if (!text) return null;
    const open = start + before.length;
    // The closing delimiter's last character is being typed: it isn't in the doc.
    const close = end - (delimiter - 1);
    const tr = state.tr;
    if (delimiter > 1) tr.delete(close, end);
    tr.delete(open, open + delimiter);
    tr.addMark(open, open + text.length, type.create());
    tr.removeStoredMark(type);
    return tr;
  });
}

/** `[ ] ` or `[x] ` starts a checklist; after `- ` it turns that one-item list into one. */
const taskRule = new InputRule(/^\s*\[([ xX])?\]\s$/, (state, match, start, end) => {
  const checked = /x/i.test(match[1] ?? "");
  const tr = state.tr.delete(start, end);
  const $start = tr.doc.resolve(start);
  const d = $start.depth;
  if (d >= 3) {
    const item = $start.node(d - 1);
    const list = $start.node(d - 2);
    if (item.type === NODES.list_item && list.type === NODES.bullet_list && list.childCount === 1 && item.childCount === 1) {
      const at = $start.before(d - 2);
      tr.replaceWith(at, at + list.nodeSize, NODES.task_list.create(null, NODES.task_item.create({ checked }, $start.parent)));
      tr.setSelection(TextSelection.create(tr.doc, at + 3));
      return tr;
    }
  }
  const range = $start.blockRange();
  const wrapping = range && findWrapping(range, NODES.task_list);
  if (!range || !wrapping) return null;
  tr.wrap(
    range,
    wrapping.map((w) => (w.type === NODES.task_item ? { type: w.type, attrs: { checked } } : w)),
  );
  const before = tr.doc.resolve(start - 1).nodeBefore;
  if (before?.type === NODES.task_list && canJoin(tr.doc, start - 1)) tr.join(start - 1);
  return tr;
});

/** `---` on a line of its own is a divider. */
const ruleRule = new InputRule(/^---$/, (state, _match, start, end) => {
  const $start = state.doc.resolve(start);
  if ($start.parent.type !== NODES.paragraph || end !== $start.end()) return null;
  const at = $start.before();
  const tr = state.tr.replaceWith(at, $start.after(), [NODES.horizontal_rule.create(), NODES.paragraph.create()]);
  tr.setSelection(TextSelection.create(tr.doc, at + 2));
  return tr;
});

/** The Markdown-ish rules (DC-1): blocks and marks as you type. */
export function markdownRules(): Plugin {
  return inputRules({
    rules: [
      textblockTypeInputRule(/^(#{1,3})\s$/, NODES.heading, (match) => ({ level: (match[1] ?? "#").length })),
      wrappingInputRule(/^\s*>\s$/, NODES.blockquote),
      taskRule,
      wrappingInputRule(/^\s*([-+*])\s$/, NODES.bullet_list),
      wrappingInputRule(
        /^(\d+)\.\s$/,
        NODES.ordered_list,
        (match) => ({ order: Number(match[1]) }),
        (match, node) => node.childCount + Number(node.attrs.order) === Number(match[1]),
      ),
      textblockTypeInputRule(/^```([A-Za-z0-9+#._-]*)\s$/, NODES.code_block, (match) => ({ language: language(match[1]) })),
      ruleRule,
      markRule(/(^|[^*])\*\*([^*\s](?:[^*]*[^*\s])?)\*\*$/, MARKS.strong, 2),
      markRule(/(^|[^*\w])\*([^*\s](?:[^*]*[^*\s])?)\*$/, MARKS.em, 1),
      markRule(/(^|[^_\w])_([^_\s](?:[^_]*[^_\s])?)_$/, MARKS.em, 1),
      markRule(/(^|[^`])`([^`]+)`$/, MARKS.code, 1),
      markRule(/(^|[^~])~~([^~\s](?:[^~]*[^~\s])?)~~$/, MARKS.strike, 2),
    ],
  });
}

function keys(editor: Editor): Plugin[] {
  const item = (f: (t: typeof NODES.list_item) => Command) => chainCommands(f(NODES.list_item), f(NODES.task_item));
  return [
    keymap({
      "Mod-z": undoCommand,
      "Mod-y": redoCommand,
      "Mod-Shift-z": redoCommand,
      "Mod-b": toggleMark(MARKS.strong),
      "Mod-i": toggleMark(MARKS.em),
      "Mod-e": toggleMark(MARKS.code),
      "Mod-Shift-x": toggleMark(MARKS.strike),
      "Mod-k": () => editor.editLink(),
      "Mod-Alt-0": setBlockType(NODES.paragraph),
      "Mod-Alt-1": setBlockType(NODES.heading, { level: 1 }),
      "Mod-Alt-2": setBlockType(NODES.heading, { level: 2 }),
      "Mod-Alt-3": setBlockType(NODES.heading, { level: 3 }),
      "Mod-Enter": chainCommands(toggleTask, exitCode),
      "Shift-Enter": chainCommands(exitCode, hardBreak),
      Enter: chainCommands(splitListItem(NODES.list_item), splitListItem(NODES.task_item, { checked: false })),
      Tab: chainCommands(goToNextCell(1), item(sinkListItem)),
      "Shift-Tab": chainCommands(goToNextCell(-1), item(liftListItem)),
      Backspace: chainCommands(undoInputRule, plainAtStart, liftAtStart),
    }),
    keymap(baseKeymap),
  ];
}

// --- Node views ---------------------------------------------------------

/** A checklist item: a box that ticks it (when the person may edit), and its text. */
class TaskItemView implements NodeView {
  dom: HTMLElement;
  contentDOM: HTMLElement;
  #box: HTMLButtonElement;
  #node: PMNode;

  constructor(node: PMNode, view: EditorView, getPos: () => number | undefined) {
    this.#node = node;
    this.#box = h("button", { type: "button", class: "task-box", role: "checkbox", tabindex: "-1", "aria-label": "Done" });
    this.#box.addEventListener("mousedown", (e) => {
      e.preventDefault();
      const pos = getPos();
      if (!view.editable || pos === undefined) return;
      view.dispatch(view.state.tr.setNodeMarkup(pos, undefined, { ...this.#node.attrs, checked: this.#node.attrs.checked !== true }));
    });
    this.contentDOM = h("div", { class: "task-body" });
    this.dom = h("li", { class: "task-item" }, h("span", { class: "task-check", contenteditable: "false" }, this.#box), this.contentDOM);
    this.#draw();
  }

  #draw(): void {
    const checked = this.#node.attrs.checked === true;
    this.dom.dataset.checked = String(checked);
    this.#box.setAttribute("aria-checked", String(checked));
  }

  update(node: PMNode): boolean {
    if (node.type !== this.#node.type) return false;
    this.#node = node;
    this.#draw();
    return true;
  }

  stopEvent(event: Event): boolean {
    return event.target instanceof Node && this.#box.contains(event.target);
  }

  ignoreMutation(mutation: ViewMutationRecord): boolean {
    return mutation.type !== "selection" && !this.contentDOM.contains(mutation.target);
  }
}

const TONE_NAMES: Record<Tone, string> = { info: "Info", warn: "Warning", success: "Success" };

/** A callout: its mark (which, clicked, changes its kind) and its blocks. */
class CalloutView implements NodeView {
  dom: HTMLElement;
  contentDOM: HTMLElement;
  #icon: HTMLButtonElement;
  #node: PMNode;

  constructor(node: PMNode, view: EditorView, getPos: () => number | undefined) {
    this.#node = node;
    this.#icon = h("button", { type: "button", class: "callout-icon", tabindex: "-1" });
    this.#icon.addEventListener("mousedown", (e) => {
      e.preventDefault();
      const pos = getPos();
      if (!view.editable || pos === undefined) return;
      const next = TONES[(TONES.indexOf(tone(this.#node.attrs.tone)) + 1) % TONES.length];
      view.dispatch(view.state.tr.setNodeMarkup(pos, undefined, { ...this.#node.attrs, tone: next }));
    });
    this.contentDOM = h("div", { class: "callout-body" });
    this.dom = h("div", { class: "callout" }, h("span", { class: "callout-side", contenteditable: "false" }, this.#icon), this.contentDOM);
    this.#draw();
  }

  #draw(): void {
    const t = tone(this.#node.attrs.tone);
    this.dom.className = `callout ${t}`;
    this.dom.dataset.tone = t;
    this.#icon.title = `${TONE_NAMES[t]} · click to change`;
    this.#icon.setAttribute("aria-label", `${TONE_NAMES[t]} callout`);
  }

  update(node: PMNode): boolean {
    if (node.type !== this.#node.type) return false;
    this.#node = node;
    this.#draw();
    return true;
  }

  stopEvent(event: Event): boolean {
    return event.target instanceof Node && this.#icon.contains(event.target);
  }

  ignoreMutation(mutation: ViewMutationRecord): boolean {
    return mutation.type !== "selection" && !this.contentDOM.contains(mutation.target);
  }
}

/** An embedded artifact (DC-1): a card that opens it in Artifacts. Never a frame in D1. */
class ArtifactView implements NodeView {
  dom: HTMLElement;
  #node: PMNode;

  constructor(node: PMNode) {
    this.#node = node;
    this.dom = h("div", { class: "artifact-card", "data-test": "artifact-card" });
    this.#draw();
  }

  #draw(): void {
    const id = String(this.#node.attrs.id);
    const title = String(this.#node.attrs.title || "Artifact");
    const url = artifactsUrl(location, id);
    this.dom.dataset.artifact = id;
    show(
      this.dom,
      h("span", { class: "page-icon", "aria-hidden": "true" }),
      h("span", { class: "artifact-text" }, h("span", { class: "artifact-title" }, title), h("span", { class: "artifact-kind" }, url ? "Artifact · opens in Artifacts" : "Artifact · can't be found")),
      url ? h("a", { class: "button small", href: url, target: "_blank", rel: "noopener noreferrer", draggable: "false" }, "Open") : null,
    );
  }

  update(node: PMNode): boolean {
    if (node.type !== this.#node.type) return false;
    this.#node = node;
    this.#draw();
    return true;
  }

  selectNode(): void {
    this.dom.classList.add("selected");
  }

  deselectNode(): void {
    this.dom.classList.remove("selected");
  }

  stopEvent(event: Event): boolean {
    return event.target instanceof Element && event.target.closest("a") !== null;
  }

  ignoreMutation(): boolean {
    return true;
  }
}

// --- Comments ------------------------------------------------------------

interface CommentsState {
  comments: readonly Comment[];
  active: string | null;
  deco: DecorationSet;
}

const commentsKey = new PluginKey<CommentsState>("comments");

function commentsPlugin(editor: Editor): Plugin<CommentsState> {
  return new Plugin<CommentsState>({
    key: commentsKey,
    state: {
      init: () => ({ comments: [], active: null, deco: DecorationSet.empty }),
      apply(tr, prev, _old, next) {
        const meta = tr.getMeta(commentsKey) as { comments: readonly Comment[]; active: string | null } | undefined;
        // y-prosemirror's own transactions (others' edits, undo) come with
        // its mapping current: the anchors are found again. Local edits
        // just move the highlights along.
        const synced = Boolean((tr.getMeta(ySyncPluginKey) as { isChangeOrigin?: boolean } | undefined)?.isChangeOrigin);
        if (meta || synced) {
          const comments = meta?.comments ?? prev.comments;
          const active = meta ? meta.active : prev.active;
          return { comments, active, deco: editor.decorate(next.doc, comments, active) };
        }
        return tr.docChanged ? { ...prev, deco: prev.deco.map(tr.mapping, tr.doc) } : prev;
      },
    },
    props: {
      decorations: (state) => commentsKey.getState(state)?.deco,
      handleClick(view, pos) {
        const found = commentsKey.getState(view.state)?.deco.find(pos, pos) ?? [];
        const id = (found[0]?.spec as { comment?: string } | undefined)?.comment;
        if (id) editor.hooks.focusComment(id);
        return false;
      },
    },
  });
}

// --- The slash menu ----------------------------------------------------------

interface Slash {
  /** Where the "/" is. */
  from: number;
  query: string;
  index: number;
  /** Closed with Escape: stays closed until this "/" is gone. */
  closed: boolean;
}

const slashKey = new PluginKey<Slash | null>("slash");

function slashState(state: EditorState, prev: Slash | null, meta: Partial<Slash> | undefined): Slash | null {
  const sel = state.selection;
  if (!sel.empty || !(sel instanceof TextSelection)) return null;
  const $from = sel.$from;
  if (!$from.parent.isTextblock || $from.parent.type.spec.code) return null;
  const before = $from.parent.textBetween(Math.max(0, $from.parentOffset - 32), $from.parentOffset, undefined, "￼");
  const match = /(?:^|\s)\/([A-Za-z0-9]*)$/.exec(before);
  if (!match) return null;
  const query = match[1] ?? "";
  const from = $from.pos - query.length - 1;
  const same = prev && prev.from === from;
  return {
    from,
    query,
    index: meta?.index ?? (same && prev.query === query ? prev.index : 0),
    closed: meta?.closed ?? (same ? prev.closed : false),
  };
}

class SlashMenu {
  readonly el: HTMLElement;
  #editor: Editor;
  #items: SlashItem[] = [];

  constructor(editor: Editor) {
    this.#editor = editor;
    this.el = h("div", { class: "slash", role: "listbox", "aria-label": "Blocks", "data-test": "slash", hidden: true });
    this.el.addEventListener("mousedown", (e) => e.preventDefault());
    editor.wrap.append(this.el);
  }

  update(view: EditorView): void {
    const s = slashKey.getState(view.state);
    this.#items = s ? slashMatches(s.query) : [];
    if (!s || s.closed || !view.editable || !this.#items.length) {
      this.el.hidden = true;
      return;
    }
    const index = Math.min(s.index, this.#items.length - 1);
    show(
      this.el,
      ...this.#items.map((item, i) =>
        h(
          "button",
          {
            type: "button",
            class: `slash-item${i === index ? " on" : ""}`,
            role: "option",
            "aria-selected": String(i === index),
            "data-kind": item.kind,
            onclick: () => this.#editor.runSlash(item),
          },
          h("span", { class: `slash-icon ${item.kind}`, "aria-hidden": "true" }),
          h("span", { class: "slash-title" }, item.title),
          item.hint ? h("span", { class: "slash-hint" }, item.hint) : null,
        ),
      ),
    );
    this.el.hidden = false;
    const at = this.#editor.place(s.from, "below");
    const room = this.#editor.wrap.clientWidth - this.el.offsetWidth;
    this.el.style.left = `${Math.max(0, Math.min(at.left, room))}px`;
    this.el.style.top = `${at.top}px`;
    this.el.querySelector(".on")?.scrollIntoView({ block: "nearest" });
  }

  get items(): SlashItem[] {
    return this.#items;
  }

  destroy(): void {
    this.el.remove();
  }
}

function slashPlugin(editor: Editor): Plugin<Slash | null> {
  return new Plugin<Slash | null>({
    key: slashKey,
    state: {
      init: () => null,
      apply: (tr, prev, _old, next) => slashState(next, prev, tr.getMeta(slashKey) as Partial<Slash> | undefined),
    },
    view: () => editor.slash,
    props: {
      handleKeyDown(view, event) {
        const s = slashKey.getState(view.state);
        const items = editor.slash.items;
        if (!s || s.closed || !view.editable || !items.length) return false;
        const index = Math.min(s.index, items.length - 1);
        if (event.key === "ArrowDown" || event.key === "ArrowUp") {
          const step = event.key === "ArrowDown" ? 1 : -1;
          view.dispatch(view.state.tr.setMeta(slashKey, { index: (index + step + items.length) % items.length }));
          return true;
        }
        if (event.key === "Enter" || event.key === "Tab") {
          const item = items[index];
          if (item) editor.runSlash(item);
          return true;
        }
        if (event.key === "Escape") {
          view.dispatch(view.state.tr.setMeta(slashKey, { closed: true }));
          return true;
        }
        return false;
      },
    },
  });
}

// --- The bubble over a selection ------------------------------------------------

const BUBBLE_MARKS: { mark: keyof typeof MARKS; label: string; title: string }[] = [
  { mark: "strong", label: "B", title: "Bold (Ctrl+B)" },
  { mark: "em", label: "I", title: "Italic (Ctrl+I)" },
  { mark: "strike", label: "S", title: "Strikethrough (Ctrl+Shift+X)" },
  { mark: "code", label: "</>", title: "Code (Ctrl+E)" },
];

class Bubble {
  readonly el: HTMLElement;
  #editor: Editor;
  /** Editing a link: the range it goes on. */
  #linking: { from: number; to: number; href: string } | null = null;
  #input: HTMLInputElement | null = null;

  constructor(editor: Editor) {
    this.#editor = editor;
    this.el = h("div", { class: "bubble", "data-test": "bubble", hidden: true });
    this.el.addEventListener("mousedown", (e) => {
      if (!(e.target instanceof HTMLInputElement)) e.preventDefault();
    });
    editor.wrap.append(this.el);
  }

  /** Starts editing the link on the selection (or the one the cursor is in). */
  editLink(): boolean {
    const view = this.#editor.view;
    if (!view.editable) return false;
    const { from, to, empty } = view.state.selection;
    const around = linkAround(view.state);
    if (empty && !around) return false;
    this.#linking = around && (empty || (from >= around.from && to <= around.to)) ? around : { from, to, href: "" };
    this.update(view);
    this.#input?.focus();
    this.#input?.select();
    return true;
  }

  update(view: EditorView): void {
    const state = view.state;
    const sel = state.selection;
    const inside = this.el.contains(document.activeElement);
    if (this.#linking) {
      if (!inside && !view.hasFocus()) this.#linking = null;
      else return this.#drawLink(view, this.#linking);
    }
    const editable = view.editable;
    const comment = this.#editor.hooks.canComment();
    const text = sel instanceof TextSelection && !sel.empty && !(sel instanceof CellSelection);
    const selected = text && (view.hasFocus() || inside || domSelectionIn(view));
    if (selected && (editable || comment)) return this.#drawMarks(view);
    const around = sel.empty && view.hasFocus() ? linkAround(state) : null;
    if (around) return this.#drawLinkInfo(view, around);
    this.el.hidden = true;
  }

  #drawMarks(view: EditorView): void {
    const state = view.state;
    const { from, to, $from } = state.selection;
    const marks = view.editable && !$from.parent.type.spec.code;
    const button = (label: string, title: string, on: boolean, run: () => void, test?: string) =>
      h("button", { type: "button", class: `bubble-button${on ? " on" : ""}`, title, "aria-label": title, "aria-pressed": String(on), "data-test": test, onclick: run }, label);
    show(
      this.el,
      ...(marks
        ? BUBBLE_MARKS.map((b) =>
            h(
              "button",
              {
                type: "button",
                class: `bubble-button mark-${b.mark}${markActive(state, MARKS[b.mark]) ? " on" : ""}`,
                title: b.title,
                "aria-label": b.title,
                "data-mark": b.mark,
                onclick: () => {
                  toggleMark(MARKS[b.mark])(view.state, view.dispatch);
                  view.focus();
                },
              },
              b.label,
            ),
          )
        : []),
      marks ? button("Link", "Link (Ctrl+K)", markActive(state, MARKS.link), () => this.editLink(), "bubble-link") : null,
      marks && this.#editor.hooks.canComment() ? h("span", { class: "bubble-divider" }) : null,
      this.#editor.hooks.canComment()
        ? button(
            "Comment",
            "Comment on this",
            false,
            () => {
              const at = this.#editor.anchorFor(from, to);
              if (at) this.#editor.hooks.comment(at);
            },
            "bubble-comment",
          )
        : null,
    );
    this.#place(view, from, to);
  }

  #drawLink(view: EditorView, linking: { from: number; to: number; href: string }): void {
    const problem = h("span", { class: "bubble-problem", hidden: true }, "That isn't a link");
    const input = h("input", {
      class: "bubble-input",
      name: "href",
      autocomplete: "off",
      spellcheck: "false",
      placeholder: "Paste a link, or a doc's link",
      "aria-label": "Link",
      value: linking.href.startsWith("#") ? linking.href.slice(1) : linking.href,
    });
    const apply = () => {
      const href = linkFrom(input.value);
      if (!href) {
        problem.hidden = false;
        return;
      }
      const tr = view.state.tr.removeMark(linking.from, linking.to, MARKS.link).addMark(linking.from, linking.to, MARKS.link.create({ href }));
      this.#linking = null;
      view.dispatch(tr);
      view.focus();
    };
    const remove = () => {
      this.#linking = null;
      view.dispatch(view.state.tr.removeMark(linking.from, linking.to, MARKS.link));
      view.focus();
    };
    input.addEventListener("keydown", (e) => {
      if (e.key === "Enter") {
        e.preventDefault();
        apply();
      } else if (e.key === "Escape") {
        e.preventDefault();
        this.#linking = null;
        view.focus();
        this.update(view);
      }
    });
    input.addEventListener("blur", () => {
      queueMicrotask(() => {
        if (this.#linking && !this.el.contains(document.activeElement) && !view.hasFocus()) {
          this.#linking = null;
          this.update(view);
        }
      });
    });
    show(
      this.el,
      input,
      problem,
      h("button", { type: "button", class: "bubble-button", "data-test": "link-apply", onclick: apply }, "Apply"),
      linking.href ? h("button", { type: "button", class: "bubble-button", onclick: remove }, "Remove") : null,
    );
    this.#input = input;
    this.#place(view, linking.from, linking.to);
  }

  #drawLinkInfo(view: EditorView, link: { from: number; to: number; href: string }): void {
    const href = safeHref(link.href);
    const doc = href ? deepLink(href) : null;
    const name = doc ? (this.#editor.hooks.titleOf(doc) ?? "A doc") : (href ?? "A link that can't be opened");
    show(
      this.el,
      h("span", { class: `bubble-href${doc ? " doc" : ""}`, title: href ?? "" }, name),
      href ? h("button", { type: "button", class: "bubble-button", onclick: () => this.#editor.follow(href) }, "Open") : null,
      view.editable ? h("button", { type: "button", class: "bubble-button", onclick: () => this.editLink() }, "Edit") : null,
      view.editable
        ? h(
            "button",
            {
              type: "button",
              class: "bubble-button",
              onclick: () => {
                view.dispatch(view.state.tr.removeMark(link.from, link.to, MARKS.link));
                view.focus();
              },
            },
            "Remove",
          )
        : null,
    );
    this.#place(view, link.from, link.to);
  }

  #place(view: EditorView, from: number, to: number): void {
    this.el.hidden = false;
    const editor = this.#editor;
    const start = editor.place(from, "above");
    const end = editor.place(to, "above");
    const center = start.top === end.top ? (start.left + end.left) / 2 : start.left;
    const width = this.el.offsetWidth;
    const left = Math.max(0, Math.min(center - width / 2, editor.wrap.clientWidth - width));
    let top = start.top - this.el.offsetHeight - 8;
    if (top < -editor.wrap.offsetTop + 4) top = editor.place(to, "below").top + 6;
    this.el.style.left = `${left}px`;
    this.el.style.top = `${top}px`;
    void view;
  }

  destroy(): void {
    this.el.remove();
  }
}

function domSelectionIn(view: EditorView): boolean {
  const sel = document.getSelection();
  return Boolean(sel && !sel.isCollapsed && sel.anchorNode && view.dom.contains(sel.anchorNode));
}

// --- Table tools ---------------------------------------------------------------

class TableTools {
  readonly el: HTMLElement;
  #editor: Editor;

  constructor(editor: Editor) {
    this.#editor = editor;
    const tool = (label: string, title: string, command: Command, test: string) =>
      h(
        "button",
        {
          type: "button",
          class: "table-tool",
          title,
          "data-test": test,
          onclick: () => {
            const view = this.#editor.view;
            command(view.state, view.dispatch);
            view.focus();
          },
        },
        label,
      );
    this.el = h(
      "div",
      { class: "table-tools", "data-test": "table-tools", hidden: true },
      tool("+ Row", "Add a row below", addRowAfter, "add-row"),
      tool("+ Column", "Add a column to the right", addColumnAfter, "add-column"),
      tool("− Row", "Remove this row", deleteRow, "remove-row"),
      tool("− Column", "Remove this column", deleteColumn, "remove-column"),
      tool("Header", "Make the first row a header, or not", toggleHeaderRow, "toggle-header"),
      tool("Delete table", "Remove the table", deleteTable, "delete-table"),
    );
    this.el.addEventListener("mousedown", (e) => e.preventDefault());
    editor.wrap.append(this.el);
  }

  update(view: EditorView): void {
    if (!view.editable || !isInTable(view.state)) {
      this.el.hidden = true;
      return;
    }
    const $from = view.state.selection.$from;
    let pos = -1;
    for (let d = $from.depth; d > 0; d--) {
      if ($from.node(d).type === NODES.table) {
        pos = $from.before(d);
        break;
      }
    }
    const dom = pos >= 0 ? view.nodeDOM(pos) : null;
    if (!(dom instanceof HTMLElement)) {
      this.el.hidden = true;
      return;
    }
    this.el.hidden = false;
    const wrap = this.#editor.wrap.getBoundingClientRect();
    const r = dom.getBoundingClientRect();
    this.el.style.left = `${Math.round(r.left - wrap.left)}px`;
    this.el.style.top = `${Math.round(r.top - wrap.top - this.el.offsetHeight - 6)}px`;
  }

  destroy(): void {
    this.el.remove();
  }
}

// --- Placeholders ------------------------------------------------------------------

const placeholders = new Plugin({
  props: {
    decorations(state) {
      const doc = state.doc;
      const first = doc.firstChild;
      if (doc.childCount === 1 && first?.type === NODES.paragraph && first.content.size === 0) {
        return DecorationSet.create(doc, [Decoration.node(0, first.nodeSize, { class: "placeholder", "data-placeholder": "Start writing, or press / for blocks" })]);
      }
      const { $from, empty } = state.selection;
      if (empty && $from.parent.type === NODES.paragraph && $from.parent.content.size === 0 && $from.depth >= 1) {
        return DecorationSet.create(doc, [Decoration.node($from.before(), $from.after(), { class: "placeholder line", "data-placeholder": "Press / for blocks" })]);
      }
      return null;
    },
  },
});

// --- The editor ------------------------------------------------------------------------

export class Editor {
  readonly view: EditorView;
  readonly wrap: HTMLElement;
  readonly hooks: Hooks;
  readonly slash: SlashMenu;
  readonly #ydoc: Y.Doc;
  readonly #fragment: Y.XmlFragment;
  #bubble: Bubble;
  #tables: TableTools;
  #onSelection = () => this.#bubble.update(this.view);

  constructor(mount: HTMLElement, ydoc: Y.Doc, hooks: Hooks) {
    this.hooks = hooks;
    this.#ydoc = ydoc;
    this.#fragment = ydoc.getXmlFragment(FRAGMENT);
    this.wrap = h("div", { class: "editor-wrap" });
    mount.append(this.wrap);
    this.slash = new SlashMenu(this);
    this.#bubble = new Bubble(this);
    this.#tables = new TableTools(this);
    const editor = this;
    const config = {
      schema,
      plugins: [
        ySyncPlugin(this.#fragment),
        yUndoPlugin(),
        slashPlugin(this),
        ...keys(this),
        markdownRules(),
        dropCursor({ color: "#3d6bff", width: 2 }),
        gapCursor(),
        tableEditing(),
        commentsPlugin(this),
        placeholders,
        new Plugin({
          view: () => ({
            update: (view) => {
              editor.#bubble.update(view);
              editor.#tables.update(view);
            },
          }),
        }),
      ],
    };
    // The sync plugin draws the document from the Yjs one when the view starts.
    this.view = new EditorView(this.wrap, {
      state: EditorState.create(config),
      editable: () => hooks.editable(),
      attributes: { class: "doc-body", "data-test": "editor", spellcheck: "true", "aria-label": "The doc" },
      nodeViews: {
        task_item: (node, view, getPos) => new TaskItemView(node, view, getPos),
        callout: (node, view, getPos) => new CalloutView(node, view, getPos),
        artifact: (node) => new ArtifactView(node),
      },
      handleDOMEvents: {
        click: (view, event) => this.#click(view, event),
        focus: () => {
          this.#onSelection();
          return false;
        },
        blur: () => {
          queueMicrotask(this.#onSelection);
          return false;
        },
      },
    });
    document.addEventListener("selectionchange", this.#onSelection);
  }

  /** Links: another doc opens here; the web opens apart. In an editable doc only with Ctrl or Cmd (or from the bubble). */
  #click(view: EditorView, event: MouseEvent): boolean {
    const target = event.target instanceof Element ? event.target.closest("a") : null;
    if (!target || !view.dom.contains(target) || target.closest(".artifact-card")) return false;
    event.preventDefault();
    if (view.editable && !(event.ctrlKey || event.metaKey)) return false;
    const href = safeHref(target.getAttribute("href"));
    if (href) this.follow(href);
    return true;
  }

  follow(href: string): void {
    const doc = deepLink(href);
    if (doc) this.hooks.openDoc(doc);
    else window.open(href, "_blank", "noopener,noreferrer");
  }

  editLink(): boolean {
    return this.#bubble.editLink();
  }

  /** Where `pos` is in the editor's own box, above or below its line. */
  place(pos: number, side: "above" | "below"): { left: number; top: number } {
    const box = this.wrap.getBoundingClientRect();
    const c = this.view.coordsAtPos(Math.max(0, Math.min(pos, this.view.state.doc.content.size)));
    return { left: Math.round(c.left - box.left), top: Math.round((side === "above" ? c.top : c.bottom) - box.top) };
  }

  /** Runs a slash item: the "/query" goes, then the block changes (or a picker opens). */
  runSlash(item: SlashItem): void {
    const view = this.view;
    const s = slashKey.getState(view.state);
    if (!s) return;
    view.dispatch(view.state.tr.delete(s.from, view.state.selection.from));
    const command = blockCommand(item.kind);
    if (command) {
      command(view.state, view.dispatch);
      view.focus();
      return;
    }
    if (item.kind === "doc-link") {
      void this.hooks.pickDoc().then((picked) => {
        view.focus();
        if (!picked || !view.editable) return;
        const mark = MARKS.link.create({ href: `#${picked.id}` });
        view.dispatch(view.state.tr.replaceSelectionWith(schema.text(picked.title || "Untitled", [mark]), false).removeStoredMark(MARKS.link));
      });
    } else if (item.kind === "artifact") {
      void this.hooks.pickArtifact().then((picked) => {
        view.focus();
        if (!picked || !view.editable) return;
        insertBlock(NODES.artifact.create({ id: picked.id, title: picked.title }))(view.state, view.dispatch);
      });
    }
  }

  // --- Comment anchors (ADR 0035 §2: a Yjs relative position, base64) ---

  #mapping(): Map<unknown, unknown> | null {
    const binding = (ySyncPluginKey.getState(this.view.state) as { binding?: { mapping?: Map<unknown, unknown> } } | undefined)?.binding;
    return binding?.mapping ?? null;
  }

  /** Where a comment on [from, to) goes, and what it quotes. */
  anchorFor(from: number, to: number): { path: string; quote: string } | null {
    const mapping = this.#mapping();
    if (!mapping) return null;
    try {
      const position = absolutePositionToRelativePosition(from, this.#fragment, mapping as never) as Y.RelativePosition;
      return { path: encodeAnchor(position), quote: snippet(this.view.state.doc.textBetween(from, to, " ", " "), 140) };
    } catch {
      return null;
    }
  }

  /** Where a comment's anchor is now, or null when it's gone or doesn't decode. */
  positionOf(path: string | null | undefined): number | null {
    const relative = decodeAnchor(path);
    const mapping = this.#mapping();
    if (!relative || !mapping) return null;
    try {
      const pos = relativePositionToAbsolutePosition(this.#ydoc, this.#fragment, relative, mapping as never);
      return pos !== null && pos >= 0 && pos <= this.view.state.doc.content.size ? pos : null;
    } catch {
      return null;
    }
  }

  /** What a comment is about, from its anchor to the end of that line. */
  quoteOf(path: string | null | undefined): string | null {
    const pos = this.positionOf(path);
    if (pos === null) return null;
    const $p = this.view.state.doc.resolve(pos);
    if (!$p.parent.isTextblock) return null;
    return snippet($p.parent.textBetween($p.parentOffset, $p.parent.content.size, " ", " "), 90) || null;
  }

  /** The highlights for open comments: the word at each anchor, or a mark where no word is. */
  decorate(doc: PMNode, comments: readonly Comment[], active: string | null): DecorationSet {
    const decos: Decoration[] = [];
    for (const c of anchored(comments)) {
      const pos = this.positionOf(c.anchor?.path);
      if (pos === null || pos > doc.content.size) continue;
      const cls = `comment-mark${c.id === active ? " active" : ""}`;
      const $p = doc.resolve(pos);
      if ($p.parent.isTextblock) {
        const text = $p.parent.textBetween(0, $p.parent.content.size, undefined, "￼");
        const word = wordAt(text, $p.parentOffset);
        if (word) {
          decos.push(Decoration.inline($p.start() + word[0], $p.start() + word[1], { class: cls, "data-comment": c.id }, { comment: c.id }));
          continue;
        }
      }
      decos.push(
        Decoration.widget(pos, () => h("span", { class: `comment-pin${c.id === active ? " active" : ""}`, "data-comment": c.id, title: "A comment is here" }), {
          comment: c.id,
          key: `comment-${c.id}-${c.id === active ? 1 : 0}`,
          side: -1,
        }),
      );
    }
    return DecorationSet.create(doc, decos);
  }

  /** The comments to highlight, and the one shown in the panel. */
  setComments(comments: readonly Comment[], active: string | null): void {
    this.view.dispatch(this.view.state.tr.setMeta(commentsKey, { comments, active }).setMeta("addToHistory", false));
  }

  /** The editor's top in the scrolling pane, for scrolling a comment's anchor into view. */
  coordsOf(pos: number): { top: number; bottom: number } | null {
    try {
      const c = this.view.coordsAtPos(pos);
      return { top: c.top, bottom: c.bottom };
    } catch {
      return null;
    }
  }

  /** Whether the person may edit changed (role, or the system reachable): the view asks again. */
  refresh(): void {
    this.view.setProps({ editable: () => this.hooks.editable() });
  }

  focus(): void {
    this.view.focus();
  }

  destroy(): void {
    document.removeEventListener("selectionchange", this.#onSelection);
    this.view.destroy();
    this.wrap.remove();
  }
}
