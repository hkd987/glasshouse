// Small pieces every page uses. Text only, through the SDK's h(): nothing
// a device reports or a publisher wrote can become markup.

import { type Child, h } from "@glasshouse/sdk";

import { SignedOut } from "./api.ts";

/** What a page says after an action: that it's done, or why not. */
export interface Said {
  text: string;
  tone: "ok" | "bad";
}

/** What [`carryOut`] gives when the session ended meanwhile: the sign-in page is on its way, and there is nothing to draw. */
export const SIGNED_OUT: unique symbol = Symbol("signed out");

/**
 * What every page's action goes through: `work`, then the page read again
 * (`reload`); what to say after, for the page to draw with.
 *
 * Each page had its own copy, with the reading again outside the `try`:
 * when that failed (the console restarting, the network gone for a
 * moment) nothing was drawn, and on Policies every button stayed disabled
 * from before the action, for good. And a session that had ended showed
 * as the words "sign in" in a red box instead of the sign-in page.
 *
 * Here the page is always drawn: with what it had and why it couldn't be
 * read again, if it couldn't. Signed out, the router is asked to start
 * over, which shows the sign-in page.
 */
export async function carryOut(work: () => Promise<string | null>, reload: () => Promise<void>): Promise<Said | null | typeof SIGNED_OUT> {
  const signedOut = (): typeof SIGNED_OUT => {
    window.dispatchEvent(new Event("hashchange"));
    return SIGNED_OUT;
  };
  let said: Said | null = null;
  try {
    const text = await work();
    said = text ? { text, tone: "ok" } : null;
  } catch (error) {
    if (error instanceof SignedOut) return signedOut();
    said = { text: (error as Error).message, tone: "bad" };
  }
  try {
    await reload();
  } catch (error) {
    if (error instanceof SignedOut) return signedOut();
    const why = `This page couldn't be read again, so it shows what it had: ${(error as Error).message}`;
    said = { text: said ? `${said.text} ${why}` : why, tone: "bad" };
  }
  return said;
}

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

/** "90 seconds", "2 minutes", "4 hours": the largest whole unit. */
export function duration(seconds: number): string {
  if (seconds >= 3600 && seconds % 3600 === 0) return plural(seconds / 3600, "hour");
  if (seconds >= 60 && seconds % 60 === 0) return plural(seconds / 60, "minute");
  return plural(seconds, "second");
}

export function capital(text: string): string {
  return text.charAt(0).toUpperCase() + text.slice(1);
}

/** A one-line message that says what happened, or why not. */
export function notice(text: string, tone: "ok" | "bad" = "ok"): HTMLElement {
  return h("div", { class: `notice ${tone}`, role: "status" }, text);
}
