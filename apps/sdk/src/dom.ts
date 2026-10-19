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
      el.addEventListener(name.slice(2).toLowerCase(), value);
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

/** The attribution chip (AU-10): "by claude-code for demo". */
export function attributionChip(text: string): HTMLElement {
  return h("span", { class: "attribution", title: "Written by an agent for you" }, h("span", { class: "attribution-dot" }), text);
}

/** Marks the app drawn and ready to use, for the launch-time budget (UI-7). */
export function ready(): void {
  performance.mark("glass:ready");
  document.documentElement.dataset.ready = "true";
}
