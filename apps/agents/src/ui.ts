// Small pieces every screen uses. Text goes in as text only, through the
// SDK's h(): nothing an agent or the front says can become markup. Icons
// are SVG built element by element, never parsed from a string.

import { type Child, h } from "@glasshouse/sdk";

import type { AgentStatus, Reach } from "./api.ts";
import { statusWords } from "./model.ts";

const SVG = "http://www.w3.org/2000/svg";

type Shape = [string, Record<string, string>];

const ICONS: Record<string, { box: number; shapes: Shape[] }> = {
  logo: {
    box: 26,
    shapes: [
      ["rect", { x: "2", y: "2", width: "22", height: "22", rx: "6", class: "stroke" }],
      ["path", { d: "M2 13h22M13 2v22", class: "stroke" }],
    ],
  },
  plus: { box: 16, shapes: [["path", { d: "M8 3v10M3 8h10", class: "stroke round" }]] },
  back: { box: 16, shapes: [["path", { d: "M10 3L5 8l5 5", class: "stroke round" }]] },
  check: {
    box: 16,
    shapes: [
      ["circle", { cx: "8", cy: "8", r: "7", class: "fill-ok" }],
      ["path", { d: "M5 8.2l2 2 4-4.4", class: "stroke-ok round" }],
    ],
  },
  cross: {
    box: 16,
    shapes: [
      ["circle", { cx: "8", cy: "8", r: "7", class: "fill-plain" }],
      ["path", { d: "M5.5 5.5l5 5M10.5 5.5l-5 5", class: "stroke-plain round" }],
    ],
  },
  moon: { box: 22, shapes: [["path", { d: "M17 13.5A7 7 0 0 1 8.5 5a7 7 0 1 0 8.5 8.5z", class: "stroke round" }]] },
  file: { box: 14, shapes: [["path", { d: "M3 1.5h5l3 3v8H3z", class: "stroke round" }]] },
  sheet: {
    box: 20,
    shapes: [
      ["rect", { x: "3", y: "2", width: "14", height: "16", rx: "2", class: "fill-ok" }],
      ["path", { d: "M6 7h8M6 10h8M6 13h5", class: "stroke-ok round" }],
    ],
  },
  expand: { box: 16, shapes: [["path", { d: "M2 6V2h4M14 6V2h-4M2 10v4h4M14 10v4h-4", class: "stroke round" }]] },
  undo: {
    box: 16,
    shapes: [
      ["path", { d: "M5 4L2 7l3 3", class: "stroke round" }],
      ["path", { d: "M2.5 7H10a4 4 0 0 1 0 8H7", class: "stroke round" }],
    ],
  },
  info: {
    box: 16,
    shapes: [
      ["circle", { cx: "8", cy: "8", r: "6.5", class: "stroke" }],
      ["path", { d: "M8 7.2v4M8 4.8v.1", class: "stroke round" }],
    ],
  },
  lock: {
    box: 16,
    shapes: [
      ["rect", { x: "3", y: "7", width: "10", height: "7", rx: "1.5", class: "stroke" }],
      ["path", { d: "M5.5 7V5a2.5 2.5 0 0 1 5 0v2", class: "stroke" }],
    ],
  },
};

export type IconName = keyof typeof ICONS;

/** A decorative icon: hidden from screen readers, sized by its class. */
export function icon(name: IconName, extra = ""): SVGSVGElement {
  const spec = ICONS[name] ?? { box: 16, shapes: [] };
  const svg = document.createElementNS(SVG, "svg");
  svg.setAttribute("viewBox", `0 0 ${spec.box} ${spec.box}`);
  svg.setAttribute("aria-hidden", "true");
  svg.setAttribute("focusable", "false");
  svg.setAttribute("class", `icon icon-${String(name)} ${extra}`.trim());
  for (const [tag, attrs] of spec.shapes) {
    const shape = document.createElementNS(SVG, tag);
    for (const [k, v] of Object.entries(attrs)) shape.setAttribute(k, v);
    svg.appendChild(shape);
  }
  return svg;
}

export function statusChip(status: AgentStatus, big = false): HTMLElement {
  const { label, tone } = statusWords(status);
  return h("span", { class: `chip tone-${tone}${big ? " big" : ""}` }, h("span", { class: "dot", "aria-hidden": "true" }), label);
}

export function avatar(name: string): HTMLElement {
  return h("span", { class: "avatar", "aria-hidden": "true" }, Array.from(name.trim())[0]?.toUpperCase() ?? "?");
}

export function reachList(reach: Reach[]): HTMLElement {
  return h(
    "ul",
    { class: "reach" },
    ...reach.map((r) =>
      h(
        "li",
        {},
        icon(r.ok ? "check" : "cross", "reach-icon"),
        h("span", { class: "visually-hidden" }, r.ok ? "Can reach: " : "Can't reach: "),
        h("span", { class: "reach-text" }, h("strong", {}, r.text), r.sub ? h("span", { class: "muted" }, r.sub) : null),
      ),
    ),
  );
}

export function button(label: Child | Child[], onclick: (event: Event) => unknown, cls = "", attrs: Record<string, string> = {}): HTMLButtonElement {
  const children = Array.isArray(label) ? label : [label];
  return h("button", { type: "button", class: `btn ${cls}`.trim(), onclick: (e: Event) => void onclick(e), ...attrs }, ...children);
}

/** A thumbnail of its computer, drawn rather than streamed: the home
 * screen doesn't read every agent's screen. */
export function thumbnail(title: string, live: boolean, asleep: boolean, big = false): HTMLElement {
  return h(
    "span",
    { class: `thumb${big ? " big" : ""}`, "aria-hidden": "true" },
    h(
      "span",
      { class: "thumb-window" },
      h("span", { class: "thumb-bar" }, h("span", { class: "thumb-dot" }), h("span", { class: "thumb-dot" }), h("span", { class: "thumb-dot" }), h("span", { class: "thumb-title" }, title)),
      h("span", { class: "thumb-rows" }, ...[1, 2, 3, 4, 5, 6].map((n) => h("span", { class: `thumb-row r${n}` }))),
    ),
    live ? h("span", { class: "live" }, h("span", { class: "live-dot" }), "Live") : null,
    asleep ? h("span", { class: "asleep-cover" }, icon("moon", "moon"), "Asleep, saved to storage") : null,
  );
}

/** "10:41", in the person's own clock. */
const CLOCK = new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit" });
export function clock(iso: string): string {
  const when = new Date(iso);
  return Number.isNaN(when.getTime()) ? "" : CLOCK.format(when);
}

/** Says something to screen readers, politely. */
export function announce(text: string): void {
  const live = document.getElementById("live");
  if (!live || !text) return;
  // A new node each time, so the same words twice are said twice.
  live.replaceChildren(h("span", {}, text));
}

/** Redraws `container` with `draw`, keeping focus on the element with the
 * same `data-key` when the one focused is replaced. */
export function keepFocus(container: Element, draw: () => void): void {
  const active = document.activeElement;
  const key = active instanceof HTMLElement && container.contains(active) ? active.dataset.key : undefined;
  draw();
  if (key) {
    const again = container.querySelector<HTMLElement>(`[data-key="${CSS.escape(key)}"]`);
    if (again && !again.hasAttribute("disabled")) again.focus({ preventScroll: true });
  }
}

/** Moves focus to a screen's heading as it opens, for screen readers. */
export function focusHeading(root: Element): void {
  const heading = root.querySelector<HTMLElement>("h1");
  if (heading) {
    heading.tabIndex = -1;
    heading.focus({ preventScroll: true });
  }
}

/** A message that says what happened, or why not. */
export function notice(text: string, tone: "ok" | "bad" = "ok"): HTMLElement {
  return h("p", { class: `notice ${tone}`, role: tone === "bad" ? "alert" : "status" }, text);
}
