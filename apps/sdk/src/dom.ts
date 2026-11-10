// A small DOM builder. Everything an app shows from mail, events or the OS
// goes in as text, never as markup: a message can't inject a button or a
// script, and the page's CSP allows nothing but the app's own files anyway.

export type Child = Node | string | number | null | undefined | false;
export type Attrs = Record<string, string | number | boolean | EventListener | null | undefined>;

export function h<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  attrs: Attrs = {},
  ...children: Child[]
): HTMLElementTagNameMap[K] {
  const el = document.createElement(tag);
  for (const [name, value] of Object.entries(attrs)) {
    if (value === null || value === undefined || value === false) continue;
    if (name.startsWith("on") && typeof value === "function") {
      listen(el, name.slice(2).toLowerCase(), value);
    } else if (name === "class") {
      el.className = String(value);
    } else if (value === true) {
      el.setAttribute(name, "");
    } else if (typeof value !== "function") {
      el.setAttribute(name, String(value));
    }
  }
  append(el, ...children);
  return el;
}

export function append(parent: Node, ...children: Child[]): void {
  for (const child of children) {
    if (child === null || child === undefined || child === false) continue;
    parent.appendChild(typeof child === "object" ? child : document.createTextNode(String(child)));
  }
}

/** Replaces what `parent` shows. */
export function show(parent: Element, ...children: Child[]): void {
  parent.replaceChildren();
  append(parent, ...children);
}

// ---- redrawing in place --------------------------------------------------

/**
 * What an element does on each event, by the event's name. A name stays
 * once it has had a handler (as `null` when it has none now): the one
 * listener added for it stays too.
 */
const HANDLERS = Symbol("glass:handlers");
/** What a `<select>` was last drawn as choosing. */
const DRAWN = Symbol("glass:drawn");
type Listening = Element & { [HANDLERS]?: Map<string, EventListener | null>; [DRAWN]?: string };

/**
 * `handler` for `type` on `el`. One listener per event is ever added; it
 * calls whatever handler the element has now, so an element kept across a
 * redraw ([`patch`]) can be given the new drawing's handlers, which close
 * over the new state.
 */
function listen(el: Listening, type: string, handler: EventListener): void {
  let handlers = el[HANDLERS];
  if (!handlers) {
    handlers = new Map();
    el[HANDLERS] = handlers;
  }
  if (!handlers.has(type)) el.addEventListener(type, (event) => el[HANDLERS]?.get(type)?.(event));
  handlers.set(type, handler);
}

/** Elements a page keeps and draws again as themselves. */
const LASTING = new WeakSet<Node>();

/**
 * Marks `el` as one the page keeps between redraws and puts in each new
 * drawing as it is: a field with suggestions of its own, an editor.
 * [`patch`] puts it back where the drawing has it and changes nothing in
 * it, where anything else is made to match what was drawn.
 */
export function lasting<T extends Node>(el: T): T {
  LASTING.add(el);
  return el;
}

/**
 * What a node is, as far as a drawing says: its kind, and for an element
 * its `type`, `id`, `name` and every `data-` attribute (`data-key` is the
 * one to give siblings nothing else tells apart). Two drawings' nodes are
 * the same thing when these are the same; its class, text and the rest are
 * how it looks now.
 */
function identity(node: Node): string {
  if (!(node instanceof Element)) return `#${node.nodeType}`;
  const parts: string[] = [];
  for (const { name, value } of Array.from(node.attributes)) {
    if (name === "type" || name === "id" || name === "name" || name.startsWith("data-")) parts.push(`${name}=${value}`);
  }
  return parts.length === 0 ? node.tagName : `${node.tagName} ${parts.sort().join(" ")}`;
}

/**
 * Makes `old` what `next` is, in place: they are the same thing
 * ([`identity`]). What the person has put in a field stays theirs, with
 * its caret and what an input method is composing, until the page has
 * something else to say:
 *
 * - it draws another value for the field than it drew last time (what was
 *   saved, the next message's subject), or
 * - it had the field disabled and enables it again: it was carrying
 *   something out and this is the answer, so a change that was refused
 *   shows as not made.
 *
 * A checkbox has nothing half done, so it is always what was drawn. The
 * one exception is a box drawn disabled and otherwise as before: that is
 * the page carrying out the click it just had, and the box shows what the
 * person chose until the page enables it again.
 */
function become(old: Node, next: Node): void {
  if (!(old instanceof Element) || !(next instanceof Element)) {
    if (old.nodeValue !== next.nodeValue) old.nodeValue = next.nodeValue;
    return;
  }
  const answered = old.hasAttribute("disabled") && !next.hasAttribute("disabled");
  if (old instanceof HTMLInputElement && next instanceof HTMLInputElement) {
    if (old.type === "checkbox" || old.type === "radio") {
      const rechecked = old.hasAttribute("checked") !== next.hasAttribute("checked");
      if ((rechecked || !next.disabled) && old.checked !== next.checked) old.checked = next.checked;
    } else {
      const redrawn = old.getAttribute("value") !== next.getAttribute("value");
      if ((redrawn || answered) && old.value !== next.value) old.value = next.value;
    }
  } else if (old instanceof HTMLTextAreaElement && next instanceof HTMLTextAreaElement) {
    const redrawn = old.defaultValue !== next.defaultValue;
    if ((redrawn || answered) && old.value !== next.value) old.value = next.value;
  }
  for (const { name } of Array.from(old.attributes)) {
    if (!next.hasAttribute(name)) old.removeAttribute(name);
  }
  for (const { name, value } of Array.from(next.attributes)) {
    if (old.getAttribute(name) !== value) old.setAttribute(name, value);
  }
  // The new drawing's handlers, on the element that stays.
  const now = (next as Listening)[HANDLERS];
  const had = (old as Listening)[HANDLERS];
  if (had) for (const type of Array.from(had.keys())) if (!now?.get(type)) had.set(type, null);
  if (now) for (const [type, handler] of now) if (handler) listen(old, type, handler);
  becomeChildren(old, Array.from(next.childNodes));
  if (old instanceof HTMLSelectElement && next instanceof HTMLSelectElement) {
    const kept = old as Listening;
    const reselected = kept[DRAWN] !== undefined && kept[DRAWN] !== next.value;
    if ((reselected || answered) && old.value !== next.value) old.value = next.value;
    kept[DRAWN] = next.value;
  }
}

/**
 * Makes `parent`'s children the drawing's. Each drawn node takes the first
 * child not yet taken that is the same thing ([`identity`]), which then
 * becomes it; a child no drawn node is, goes. So one that comes or goes (a
 * notice above a form) leaves the rest the elements they were, and nothing
 * becomes a different thing: a field someone is typing in can't turn into
 * the field of the row that took its place.
 */
function becomeChildren(parent: Node, next: Node[]): void {
  // The page's own elements among the drawing: placed, never matched.
  const placed = new Set<Node>();
  for (const node of next) if (LASTING.has(node)) placed.add(node);
  // The children there are, by what they are, in order.
  const what = new Map<Node, string>();
  const waiting = new Map<string, ChildNode[]>();
  for (const old of Array.from(parent.childNodes)) {
    if (placed.has(old)) continue;
    const id = identity(old);
    what.set(old, id);
    const same = waiting.get(id);
    if (same) same.push(old);
    else waiting.set(id, [old]);
  }
  // How many of each the drawing still has to place.
  const ids = next.map((node) => (placed.has(node) ? null : identity(node)));
  const wanted = new Map<string, number>();
  for (const id of ids) if (id !== null) wanted.set(id, (wanted.get(id) ?? 0) + 1);

  let at: ChildNode | null = parent.firstChild;
  next.forEach((node, i) => {
    const id = ids[i];
    // Kept by the page and drawn as itself: put where the drawing has it.
    // (Drawing it into the new tree took it out of this one, unless it is
    // drawn right here.)
    if (id === null || id === undefined) {
      if (node === at) at = at.nextSibling;
      else parent.insertBefore(node, at);
      return;
    }
    // Children here that nothing left to draw is: gone from the drawing.
    // Taken out now, what follows them stays where it is and isn't moved:
    // moving an element takes the keyboard from a field in it and loads a
    // frame in it again.
    while (at && !placed.has(at) && (wanted.get(what.get(at) ?? "") ?? 0) === 0) {
      const gone: ChildNode = at;
      at = at.nextSibling;
      parent.removeChild(gone);
    }
    wanted.set(id, (wanted.get(id) ?? 1) - 1);
    const kept = waiting.get(id)?.shift();
    if (!kept) {
      parent.insertBefore(node, at);
      return;
    }
    if (kept === at) at = at.nextSibling;
    else parent.insertBefore(kept, at);
    become(kept, node);
  });
  while (at) {
    const gone: ChildNode = at;
    at = at.nextSibling;
    parent.removeChild(gone);
  }
}

/**
 * Makes `parent` show `children`, changing what is there instead of
 * replacing it ([`show`] replaces). An element stays the element it was
 * wherever the new drawing has the same thing among its siblings: the same
 * tag, `type`, `id`, `name` and `data-` attributes, the first with the
 * first. Give siblings that nothing else tells apart, and that come and go
 * (the rows of a list), a `data-key`. So a redraw that comes while someone
 * is using the page (new mail, a timer) leaves them the field they are
 * typing in with its caret, the list where they scrolled it, and a frame
 * without loading it again.
 */
export function patch(parent: Element, ...children: Child[]): void {
  const next: Node[] = [];
  for (const child of children) {
    if (child === null || child === undefined || child === false) continue;
    next.push(typeof child === "object" ? child : document.createTextNode(String(child)));
  }
  becomeChildren(parent, next);
}

/**
 * [`patch`], for a page that draws itself whole: `draw` makes the new
 * drawing and `parent` is changed to match. The field someone is typing in
 * keeps the keyboard and its caret even when it is a [`lasting`] element,
 * which drawing moves and so takes the keyboard from.
 */
export function redraw(parent: Element, draw: () => Child[]): void {
  const active = document.activeElement;
  const typing = active instanceof HTMLElement && parent.contains(active) ? active : null;
  let caret: [number, number, "forward" | "backward" | "none"] | null = null;
  if (typing instanceof HTMLInputElement || typing instanceof HTMLTextAreaElement) {
    try {
      const { selectionStart, selectionEnd, selectionDirection } = typing;
      if (selectionStart !== null && selectionEnd !== null) {
        caret = [selectionStart, selectionEnd, selectionDirection ?? "none"];
      }
    } catch {
      // A kind of field with no caret to keep (a number, a date).
    }
  }
  patch(parent, ...draw());
  if (typing && typing.isConnected && document.activeElement !== typing) {
    typing.focus({ preventScroll: true });
    if (caret && (typing instanceof HTMLInputElement || typing instanceof HTMLTextAreaElement)) {
      try {
        typing.setSelectionRange(...caret);
      } catch {
        // As above.
      }
    }
  }
}

/** The attribution chip (AU-10): "by claude-code for demo". */
export function attributionChip(text: string): HTMLElement {
  return h("span", { class: "attribution", title: "Written by an agent for you" }, h("span", { class: "attribution-dot" }), text);
}

/** Marks the app drawn and ready to use, for the launch-time budget (UI-7). */
export function ready(): void {
  performance.mark("glass:ready");
  document.documentElement.dataset.ready = "true";
}
