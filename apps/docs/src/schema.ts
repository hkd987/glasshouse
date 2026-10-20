// The document's schema (DC-1, ADR 0035 §1). Its names are a contract:
// the computer reads and writes the same Yjs document with yrs and turns it
// into Markdown and back (doc.read, doc.write), so every node and mark here
// is named exactly as glass-app-docs names it, and nothing else is in it.
//
// Through y-prosemirror each node is a Y.XmlElement named after it, its
// attributes the node's attrs (those that aren't null), and text a
// Y.XmlText whose formatting attributes are the marks, each an object of
// the mark's attrs: `{ strong: {} }`, `{ link: { href } }`, `{ ai: { by } }`.
// What the computer writes must fit these content rules too:
//
//   doc            block+
//   paragraph      inline*            heading {level 1-3}   inline*
//   blockquote     block+             callout {tone}        block+
//   bullet_list    list_item+         ordered_list {order}  list_item+
//   list_item      paragraph block*   task_list             task_item+
//   task_item {checked}  paragraph block*
//   code_block {language}  text*, no marks
//   table          table_row+         table_row             (table_cell | table_header)*
//   table_cell, table_header {colspan 1, rowspan 1, colwidth null}  block+
//   horizontal_rule, artifact {id, title}: leaves; hard_break: inline leaf
//
// A page drops an element it can't read (y-prosemirror deletes it), so a
// name outside this list loses what it holds. Attrs here are read leniently
// (no validation) so an odd value is drawn sensibly rather than dropped.

import { type DOMOutputSpec, type Mark, type MarkSpec, type Node, type NodeSpec, Schema } from "prosemirror-model";
import { tableNodes } from "prosemirror-tables";

import { isArtifactId, safeHref } from "./docs.ts";

/** The block types a callout comes in. */
export const TONES = ["info", "warn", "success"] as const;
export type Tone = (typeof TONES)[number];

export function tone(value: unknown): Tone {
  return TONES.includes(value as Tone) ? (value as Tone) : "info";
}

/** A heading's level, 1 to 3, whatever was written. */
export function level(value: unknown): 1 | 2 | 3 {
  const n = Math.round(Number(value));
  return n >= 3 ? 3 : n === 2 ? 2 : 1;
}

/** A code block's language: a short word, or nothing. */
export function language(value: unknown): string {
  return typeof value === "string" && /^[A-Za-z0-9+#._-]{1,32}$/.test(value) ? value : "";
}

const element = (dom: HTMLElement | string) => (typeof dom === "string" ? null : dom);

const nodes: Record<string, NodeSpec> = {
  doc: { content: "block+" },
  paragraph: {
    content: "inline*",
    group: "block",
    parseDOM: [{ tag: "p" }],
    toDOM: () => ["p", 0],
  },
  heading: {
    attrs: { level: { default: 1 } },
    content: "inline*",
    group: "block",
    defining: true,
    parseDOM: [
      { tag: "h1", attrs: { level: 1 } },
      { tag: "h2", attrs: { level: 2 } },
      { tag: "h3", attrs: { level: 3 } },
      { tag: "h4", attrs: { level: 3 } },
      { tag: "h5", attrs: { level: 3 } },
      { tag: "h6", attrs: { level: 3 } },
    ],
    toDOM: (node) => [`h${level(node.attrs.level)}`, 0],
  },
  blockquote: {
    content: "block+",
    group: "block",
    defining: true,
    parseDOM: [{ tag: "blockquote" }],
    toDOM: () => ["blockquote", 0],
  },
  bullet_list: {
    content: "list_item+",
    group: "block",
    parseDOM: [{ tag: "ul" }],
    toDOM: () => ["ul", 0],
  },
  ordered_list: {
    attrs: { order: { default: 1 } },
    content: "list_item+",
    group: "block",
    parseDOM: [
      {
        tag: "ol",
        getAttrs: (dom) => {
          const start = element(dom)?.getAttribute("start");
          return { order: start && /^\d+$/.test(start) ? Number(start) : 1 };
        },
      },
    ],
    toDOM: (node) => {
      const order = Math.max(0, Math.round(Number(node.attrs.order) || 1));
      return ["ol", order === 1 ? {} : { start: order }, 0];
    },
  },
  list_item: {
    content: "paragraph block*",
    defining: true,
    parseDOM: [{ tag: "li" }],
    toDOM: () => ["li", 0],
  },
  task_list: {
    content: "task_item+",
    group: "block",
    parseDOM: [{ tag: "ul[data-type=task_list]", priority: 60 }],
    toDOM: () => ["ul", { "data-type": "task_list", class: "task-list" }, 0],
  },
  task_item: {
    attrs: { checked: { default: false } },
    content: "paragraph block*",
    defining: true,
    parseDOM: [
      {
        tag: "li[data-checked]",
        priority: 60,
        getAttrs: (dom) => ({ checked: element(dom)?.getAttribute("data-checked") === "true" }),
      },
    ],
    toDOM: (node) => ["li", { class: "task-item", "data-checked": node.attrs.checked === true ? "true" : "false" }, 0],
  },
  code_block: {
    attrs: { language: { default: "" } },
    content: "text*",
    marks: "",
    group: "block",
    code: true,
    defining: true,
    parseDOM: [
      {
        tag: "pre",
        preserveWhitespace: "full",
        getAttrs: (dom) => ({ language: language(element(dom)?.getAttribute("data-language")) }),
      },
    ],
    toDOM: (node) => {
      const lang = language(node.attrs.language);
      return ["pre", lang ? { "data-language": lang } : {}, ["code", 0]];
    },
  },
  callout: {
    attrs: { tone: { default: "info" } },
    content: "block+",
    group: "block",
    defining: true,
    parseDOM: [{ tag: "div[data-tone]", getAttrs: (dom) => ({ tone: tone(element(dom)?.getAttribute("data-tone")) }) }],
    toDOM: (node) => ["div", { class: `callout ${tone(node.attrs.tone)}`, "data-tone": tone(node.attrs.tone) }, 0],
  },
  horizontal_rule: {
    group: "block",
    parseDOM: [{ tag: "hr" }],
    toDOM: () => ["hr"],
  },
  ...tableNodes({ tableGroup: "block", cellContent: "block+", cellAttributes: {} }),
  artifact: {
    attrs: { id: { default: "" }, title: { default: "" } },
    group: "block",
    atom: true,
    selectable: true,
    draggable: true,
    parseDOM: [
      {
        tag: "div[data-artifact]",
        getAttrs: (dom) => {
          const el = element(dom);
          const id = el?.getAttribute("data-artifact");
          return isArtifactId(id) ? { id, title: el?.getAttribute("data-title") ?? "" } : false;
        },
      },
    ],
    // The page draws a card that opens Artifacts (editor.ts); this is
    // what a copy carries.
    toDOM: (node) => [
      "div",
      { class: "artifact-embed", "data-artifact": String(node.attrs.id), "data-title": String(node.attrs.title) },
      String(node.attrs.title || "Artifact"),
    ],
  },
  text: { group: "inline" },
  hard_break: {
    inline: true,
    group: "inline",
    selectable: false,
    parseDOM: [{ tag: "br" }],
    toDOM: () => ["br"],
  },
};

/** A link's place in the page: only the kinds `safeHref` allows, opened apart from this one. */
function linkDOM(mark: Mark): DOMOutputSpec {
  const href = safeHref(mark.attrs.href);
  if (!href) return ["span", { class: "link-refused", title: "This link can't be opened from here" }, 0];
  return href.startsWith("#") ? ["a", { href, class: "doc-link" }, 0] : ["a", { href, rel: "noopener noreferrer", target: "_blank" }, 0];
}

const marks: Record<string, MarkSpec> = {
  link: {
    attrs: { href: { default: "" } },
    inclusive: false,
    parseDOM: [
      {
        tag: "a[href]",
        getAttrs: (dom) => {
          const href = safeHref(element(dom)?.getAttribute("href"));
          return href ? { href } : false;
        },
      },
    ],
    toDOM: linkDOM,
  },
  // Text an agent or the AI wrote (DC-2): drawn faintly, named on hover.
  ai: {
    attrs: { by: { default: "" } },
    inclusive: false,
    parseDOM: [{ tag: "span[data-ai]", getAttrs: (dom) => ({ by: element(dom)?.getAttribute("data-ai") ?? "" }) }],
    toDOM: (mark) => {
      const by = String(mark.attrs.by || "an agent");
      return ["span", { class: "ai-text", "data-ai": by, title: `Written by ${by}` }, 0];
    },
  },
  strong: {
    parseDOM: [
      { tag: "strong" },
      { tag: "b", getAttrs: (dom) => (element(dom)?.style.fontWeight !== "normal" ? null : false) },
      { style: "font-weight=bold" },
      { style: "font-weight", getAttrs: (value) => (/^(bold(er)?|[6-9]\d{2,})$/.test(String(value)) ? null : false) },
    ],
    toDOM: () => ["strong", 0],
  },
  em: {
    parseDOM: [{ tag: "em" }, { tag: "i" }, { style: "font-style=italic" }],
    toDOM: () => ["em", 0],
  },
  strike: {
    parseDOM: [{ tag: "s" }, { tag: "del" }, { tag: "strike" }, { style: "text-decoration=line-through" }],
    toDOM: () => ["s", 0],
  },
  code: {
    parseDOM: [{ tag: "code" }],
    toDOM: () => ["code", 0],
  },
};

export const schema = new Schema({ nodes, marks });

/** The node and mark names the computer knows (ADR 0035 §1), for the contract test. */
export const NODE_NAMES = [
  "doc",
  "paragraph",
  "heading",
  "blockquote",
  "bullet_list",
  "ordered_list",
  "list_item",
  "task_list",
  "task_item",
  "code_block",
  "callout",
  "horizontal_rule",
  "table",
  "table_row",
  "table_cell",
  "table_header",
  "artifact",
  "text",
  "hard_break",
] as const;
export const MARK_NAMES = ["link", "ai", "strong", "em", "strike", "code"] as const;

/** Whether `node` is a list item of either kind. */
export function isItem(node: Node): boolean {
  return node.type === schema.nodes.list_item || node.type === schema.nodes.task_item;
}
