// Small pieces every page uses. Text only, through the SDK's h(): nothing
// a device reports or a publisher wrote can become markup.

import { type Child, h } from "@glasshouse/sdk";

export function card(title: string | null, subtitle: string | null, ...children: Child[]): HTMLElement {
  return h(
    "section",
    { class: "card" },
    title ? h("h2", {}, title) : null,
    subtitle ? h("p", { class: "card-sub" }, subtitle) : null,
    ...children,
  );
}

export function chip(text: string, tone: "ok" | "wait" | "bad" | "plain" = "plain"): HTMLElement {
  return h("span", { class: `chip ${tone}` }, text);
}

export function mono(text: string): HTMLElement {
  return h("span", { class: "mono" }, text);
}

export function stat(label: string, value: number | string, dot: "blue" | "orange" | null = null): HTMLElement {
  return h(
    "div",
    { class: "stat" },
    h("span", { class: "stat-label" }, label),
    h("span", { class: "stat-value" }, dot ? h("span", { class: `dot ${dot}` }) : null, String(value)),
  );
}

export function plural(n: number, one: string, many = `${one}s`): string {
  return `${n.toLocaleString("en-US")} ${n === 1 ? one : many}`;
}

/** "3 min ago", from an RFC 3339 time. */
export function ago(when: string): string {
  const seconds = Math.max(0, Math.round((Date.now() - Date.parse(when)) / 1000));
  if (seconds < 60) return "just now";
  if (seconds < 3600) return `${Math.round(seconds / 60)} min ago`;
  if (seconds < 86400) return `${Math.round(seconds / 3600)} h ago`;
  return `${Math.round(seconds / 86400)} d ago`;
}

export function capital(text: string): string {
  return text.charAt(0).toUpperCase() + text.slice(1);
}

/** A one-line message that says what happened, or why not. */
export function notice(text: string, tone: "ok" | "bad" = "ok"): HTMLElement {
  return h("div", { class: `notice ${tone}`, role: "status" }, text);
}
