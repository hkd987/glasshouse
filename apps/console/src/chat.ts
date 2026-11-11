// Chat (ADR 0030 §4): how long the org keeps chat (CF-61), and the
// compliance key every computer seals chat to, by its fingerprint.
//
// The periods are not policy: they go out in IT's Chat release, signed, to
// every computer at once, with no rings. One organization has one rule for
// how long a message exists. Removing is for good, so a change is asked
// first without doing it: the console says how many messages it holds that
// would go, and only "Remove and keep to this" sets it.

import { h, show } from "@glasshouse/sdk";

import { type ChatPage, api } from "./api.ts";
import { ago, card, carryOut, mono, notice, plural, SIGNED_OUT } from "./ui.ts";

interface Form {
  channel: string;
  dm: string;
}

/** A field's days: empty is for good (0); anything else a whole number of days. */
function days(text: string, what: string, max: number): number {
  const trimmed = text.trim();
  if (trimmed === "") return 0;
  if (!/^\d+$/.test(trimmed) || Number(trimmed) < 1) throw new Error(`${what}: a whole number of days, or empty to keep them for good.`);
  if (Number(trimmed) > max) throw new Error(`${what}: the longest is ${max.toLocaleString("en-US")} days. Leave it empty to keep them for good.`);
  return Number(trimmed);
}

function kept(n: number): string {
  return n === 0 ? "for good" : n === 1 ? "for a day" : `for ${n.toLocaleString("en-US")} days`;
}

export async function chatPage(root: HTMLElement): Promise<void> {
  let page = await api<ChatPage>("GET", "/api/chat");
  const field = (n: number): string => (n === 0 ? "" : String(n));
  const form: Form = { channel: field(page.kept_for.channel_days), dm: field(page.kept_for.dm_days) };
  /** What IT asked for and what it would remove, waiting on a yes. */
  let asked: { channel_days: number; dm_days: number; would_remove: number } | null = null;
  let message: { text: string; tone: "ok" | "bad" } | null = null;

  async function act(work: () => Promise<string | null>): Promise<void> {
    const said = await carryOut(work, async () => {
      page = await api<ChatPage>("GET", "/api/chat");
    });
    if (said === SIGNED_OUT) return;
    message = said;
    draw();
  }

  function input(label: string, name: keyof Form): HTMLElement {
    return h(
      "label",
      { class: "field" },
      label,
      h(
        "span",
        { class: "days" },
        h("input", {
          class: "text short",
          name: `${name}-days`,
          inputmode: "numeric",
          placeholder: "for good",
          value: form[name],
          oninput: (e: Event) => {
            form[name] = (e.target as HTMLInputElement).value;
            // What was asked is no longer what the fields say.
            if (asked) {
              asked = null;
              draw();
            }
          },
        }),
        h("span", { class: "hint" }, "days"),
      ),
    );
  }

  function confirm(): HTMLElement | null {
    if (!asked) return null;
    const a = asked;
    const none = a.channel_days === 0 && a.dm_days === 0;
    return h(
      "div",
      { class: "confirm", "data-test": "confirm" },
      h("p", {}, h("strong", {}, `Channels are kept ${kept(a.channel_days)}; direct messages and groups ${kept(a.dm_days)}.`)),
      h(
        "p",
        {},
        none
          ? "Nothing more is removed from here on. What was already removed stays removed."
          : a.would_remove === 0
            ? "No message the console holds is old enough to go today. From here on, each is removed from every computer and from the console as it passes its period."
            : `${plural(a.would_remove, "message")} the console holds ${a.would_remove === 1 ? "is" : "are"} older than that. They are removed from every computer and from the console now, and can't be brought back.`,
      ),
      h(
        "div",
        { class: "actions" },
        h(
          "button",
          {
            class: none || a.would_remove === 0 ? "button primary" : "button danger",
            "data-test": "keep-confirm",
            onclick: () =>
              act(async () => {
                const r = await api<{ version: number }>("POST", "/api/chat/keep", { channel_days: a.channel_days, dm_days: a.dm_days, confirm: true });
                asked = null;
                return `Set, as Chat v${r.version}. Every computer applies it in about a second, and one that is off when it next starts.`;
              }),
          },
          none ? "Keep chat for good" : a.would_remove === 0 ? "Keep to this" : "Remove and keep to this",
        ),
        h(
          "button",
          {
            class: "button",
            onclick: () => {
              asked = null;
              draw();
            },
          },
          "Cancel",
        ),
      ),
    );
  }

  function draw(): void {
    const now = page.kept_for;
    show(
      root,
      h(
        "div",
        { class: "page" },
        h(
          "header",
          { class: "page-head" },
          h(
            "div",
            {},
            h("div", { class: "crumbs" }, "Chat"),
            h("h1", {}, "Chat"),
            h("p", { class: "sub" }, "How long the organization keeps chat. It is one rule for everyone, so it reaches every computer at once, not ring by ring."),
          ),
        ),
        message ? notice(message.text, message.tone) : null,
        h(
          "div",
          { class: "columns" },
          h(
            "div",
            { class: "main" },
            card(
              "How long chat is kept",
              "Messages older than this are removed from every computer and from the console. Leave a field empty to keep those messages for good.",
              h(
                "p",
                { class: "hint", "data-test": "kept-now" },
                `Now: channels are kept ${kept(now.channel_days)}; direct messages and groups ${kept(now.dm_days)}.`,
                page.set ? ` Set by ${page.set.by}, ${ago(page.set.at)}.` : "",
              ),
              input("Channels", "channel"),
              input("Direct messages and groups", "dm"),
              h(
                "div",
                { class: "actions" },
                h(
                  "button",
                  {
                    class: "button primary",
                    "data-test": "keep-ask",
                    onclick: () =>
                      act(async () => {
                        const channel_days = days(form.channel, "Channels", page.max_days);
                        const dm_days = days(form.dm, "Direct messages and groups", page.max_days);
                        if (channel_days === now.channel_days && dm_days === now.dm_days) {
                          asked = null;
                          return "That's how long chat is kept now.";
                        }
                        const r = await api<{ would_remove: number }>("POST", "/api/chat/keep", { channel_days, dm_days });
                        asked = { channel_days, dm_days, would_remove: r.would_remove };
                        return null;
                      }),
                  },
                  "See what this removes",
                ),
              ),
              confirm(),
            ),
          ),
          h(
            "div",
            { class: "side" },
            card(
              "What this covers",
              null,
              h(
                "ul",
                { class: "changes" },
                h("li", {}, "Channels, direct messages and groups: messages, replies, reactions, edits, pins and topics."),
                h("li", {}, "Not docs, artifacts, skills or meetings. Their content isn't chat, and stays."),
                h("li", {}, "A computer that is off removes what's past its period when it next starts, with or without the console in reach."),
                h("li", {}, "Each computer records how many messages it removed in its audit log, never which."),
              ),
            ),
            card(
              "Before a legal hold",
              null,
              h(
                "p",
                { "data-test": "no-holds" },
                "Holds aren't built yet: nothing can be kept past its period for a matter. If the organization may need to preserve chat, keep it for good until they are.",
              ),
            ),
            card(
              "Compliance key",
              "Every computer seals chat to this key as well as to its members, and shows the same fingerprint in Chat.",
              h("p", { "data-test": "fingerprint" }, page.fingerprint ? mono(page.fingerprint) : "Not set up yet."),
              page.version ? h("p", { class: "hint" }, "Chat ", mono(`v${page.version}`)) : null,
            ),
          ),
        ),
      ),
    );
  }
  draw();
}
