// Chat's logic (CF-92, CF-93, ADR 0030 C4a): the @-autocomplete, mention
// chips, the "New messages" divider, thread summaries, reaction pills,
// search hits and the sidebar's badges.

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  type Conversation,
  type Hit,
  type Message,
  ago,
  dayLabel,
  draftKey,
  editable,
  firstUnread,
  groupHits,
  insertMention,
  joins,
  lastEditable,
  matchSegments,
  mentionMatches,
  mentionQuery,
  mentionSegments,
  MORE_EMOJI,
  QUICK_EMOJI,
  reactionPills,
  rowBadges,
  threadSummary,
} from "../src/chat.ts";

const NOW = new Date("2026-10-20T15:00:00");

function message(over: Partial<Message> & { id: string }): Message {
  return {
    seq: 1,
    sender: "priya",
    sender_name: "Priya Shah",
    mine: false,
    at: "2026-10-20T14:00:00",
    text: "Kickoff at 3",
    attribution: null,
    ...over,
  };
}

const PEOPLE = [
  { login: "alex", name: "Alex Morgan" },
  { login: "priya", name: "Priya Shah" },
  { login: "priyanka", name: "Priyanka Rao" },
  { login: "sam", name: "Sam Okafor" },
  { login: "samantha", name: "Samantha Lee" },
];

test("cf92 an @ starts a mention at the start or after a space, never inside an address", () => {
  assert.deepEqual(mentionQuery("@pri", 4), { start: 0, end: 4, query: "pri" });
  assert.deepEqual(mentionQuery("hi @", 4), { start: 3, end: 4, query: "" });
  assert.deepEqual(mentionQuery("ask (@sa", 8), { start: 5, end: 8, query: "sa" });
  // The caret inside a login: the whole login is replaced.
  assert.deepEqual(mentionQuery("hi @priya there", 6), { start: 3, end: 9, query: "pr" });
  assert.equal(mentionQuery("mail priya@northwind.example", 16), null);
  assert.equal(mentionQuery("no mention here", 5), null);
  assert.equal(mentionQuery("@@x", 3), null);
  // Past the space after a mention, there is none.
  assert.equal(mentionQuery("@priya ", 7), null);
});

test("cf92 mention suggestions are the conversation's people, logins first, never the person", () => {
  assert.deepEqual(
    mentionMatches(PEOPLE, "pri", "alex").map((p) => p.login),
    ["priya", "priyanka"],
  );
  // By any word of the name.
  assert.deepEqual(
    mentionMatches(PEOPLE, "lee", "alex").map((p) => p.login),
    ["samantha"],
  );
  assert.deepEqual(
    mentionMatches(PEOPLE, "", "alex").map((p) => p.login),
    ["priya", "priyanka", "sam", "samantha"],
  );
  assert.deepEqual(mentionMatches(PEOPLE, "al", "alex"), []);
  assert.equal(mentionMatches(PEOPLE, "", "nobody", 2).length, 2);
  assert.deepEqual(
    mentionMatches(PEOPLE, "PRI", "alex").map((p) => p.login),
    ["priya", "priyanka"],
  );
});

test("cf92 choosing a suggestion puts @login and a space in, the caret after it", () => {
  assert.deepEqual(insertMention("hi @pri", 3, 7, "priya"), { text: "hi @priya ", caret: 10 });
  assert.deepEqual(insertMention("hi @pr there", 3, 6, "priya"), { text: "hi @priya there", caret: 10 });
  assert.deepEqual(insertMention("@", 0, 1, "sam"), { text: "@sam ", caret: 5 });
});

test("cf92 mentions glassd found show as chips; the longest login wins and lookalikes stay text", () => {
  const sam = { login: "sam", name: "Sam Okafor" };
  const samantha = { login: "samantha", name: "Samantha Lee" };
  assert.deepEqual(mentionSegments("@samantha and @sam, see this", [sam, samantha]), [
    { text: "@samantha", mention: samantha },
    { text: " and " },
    { text: "@sam", mention: sam },
    { text: ", see this" },
  ]);
  // Not mentioned by glassd, not an address, not part of a longer login: text.
  assert.deepEqual(mentionSegments("mail sam@northwind.example or @sammy or @lee", [sam]), [{ text: "mail sam@northwind.example or @sammy or @lee" }]);
  // At the end of a sentence it is still a mention.
  assert.deepEqual(mentionSegments("Thanks @sam.", [sam]), [{ text: "Thanks " }, { text: "@sam", mention: sam }, { text: "." }]);
  assert.deepEqual(mentionSegments("plain", undefined), [{ text: "plain" }]);
  assert.deepEqual(mentionSegments("", [sam]), []);
  // Text that looks like a pattern is only text.
  const odd = { login: "a.b", name: "A B" };
  assert.deepEqual(mentionSegments("@a.b and @aXb", [odd]), [{ text: "@a.b", mention: odd }, { text: " and @aXb" }]);
});

test("cf92 the New messages divider goes before the earliest unread message from others", () => {
  const timeline = [
    message({ id: "m1" }),
    message({ id: "m2", mine: true, sender: "alex" }),
    message({ id: "m3" }),
    message({ id: "m4", mine: true, sender: "alex" }),
    message({ id: "m5" }),
  ];
  assert.equal(firstUnread(timeline, 0), null);
  assert.equal(firstUnread(timeline, 1), "m5");
  // The person's own messages are never unread, so they are skipped.
  assert.equal(firstUnread(timeline, 2), "m3");
  // More unread than the window holds: the earliest one shown.
  assert.equal(firstUnread(timeline, 9), "m1");
  assert.equal(firstUnread([message({ id: "x", mine: true })], 3), null);
  assert.equal(firstUnread([], 3), null);
});

test("cf92 a root shows its replies, the last reply's time and up to three repliers", () => {
  assert.equal(threadSummary(message({ id: "r" }), NOW), null);
  assert.equal(threadSummary(message({ id: "r", replies: 0 }), NOW), null);
  assert.deepEqual(threadSummary(message({ id: "r", replies: 1, last_reply_at: "2026-10-20T14:58:00", repliers: ["Sam Okafor"] }), NOW), {
    count: "1 reply",
    last: "last reply 2m ago",
    text: "1 reply · last reply 2m ago",
    repliers: ["Sam Okafor"],
  });
  const many = threadSummary(message({ id: "r", replies: 12, last_reply_at: "2026-10-19T09:00:00", repliers: ["A", "B", "C", "D"] }), NOW);
  assert.equal(many?.text, "12 replies · last reply yesterday");
  assert.deepEqual(many?.repliers, ["A", "B", "C"]);
  assert.equal(threadSummary(message({ id: "r", replies: 2, last_reply_at: null }), NOW)?.text, "2 replies");
});

test("cf92 times read as people say them", () => {
  assert.equal(ago("2026-10-20T14:59:40", NOW), "just now");
  assert.equal(ago("2026-10-20T14:15:00", NOW), "45m ago");
  assert.equal(ago("2026-10-20T09:00:00", NOW), "6h ago");
  assert.equal(ago("2026-10-19T23:30:00", NOW), "yesterday");
  assert.equal(ago("2026-10-16T12:00:00", NOW), "4d ago");
  assert.equal(ago("2026-09-03T12:00:00", NOW), "on Sep 3");
  assert.equal(ago("not a time", NOW), "");
  assert.equal(dayLabel("2026-10-20T08:00:00", NOW), "Today");
  assert.equal(dayLabel("2026-10-19T08:00:00", NOW), "Yesterday");
  assert.equal(dayLabel("2026-10-16T08:00:00", NOW), "Friday, October 16");
  assert.equal(dayLabel("2025-10-16T08:00:00", NOW), "Thursday, October 16, 2025");
});

test("cf92 reaction pills show counts, the person's own, and who reacted", () => {
  const pills = reactionPills(
    [
      { emoji: "👍", count: 2, mine: true, names: ["Priya Shah", "Alex Morgan"] },
      { emoji: "🎉", count: 1, mine: false, names: ["Priya Shah"] },
      { emoji: "👀", count: 4, mine: false, names: ["Priya Shah", "Sam Okafor"] },
      { emoji: "🔥", count: 1, mine: true, names: ["Alex Morgan"] },
      { emoji: "😂", count: 0, mine: false, names: [] },
    ],
    "Alex Morgan",
  );
  assert.deepEqual(
    pills.map((p) => [p.emoji, p.count, p.mine, p.title]),
    [
      ["👍", 2, true, "Priya Shah and you reacted with 👍"],
      ["🎉", 1, false, "Priya Shah reacted with 🎉"],
      ["👀", 4, false, "Priya Shah, Sam Okafor and 2 others reacted with 👀"],
      ["🔥", 1, true, "You reacted with 🔥"],
    ],
  );
  assert.equal(pills[0]?.label, "thumbs up, 2 reactions, including yours. Remove yours");
  assert.equal(pills[1]?.label, "party, 1 reaction. Add yours");
  assert.deepEqual(reactionPills(undefined, "Alex Morgan"), []);
});

test("cf92 the picker offers 8 reactions first and about 40 more, each once", () => {
  assert.equal(QUICK_EMOJI.length, 8);
  assert.ok(MORE_EMOJI.length >= 36 && MORE_EMOJI.length <= 44, String(MORE_EMOJI.length));
  const all = [...QUICK_EMOJI, ...MORE_EMOJI].map(([e]) => e);
  assert.equal(new Set(all).size, all.length);
});

test("cf92 Up edits the person's last message they wrote themselves", () => {
  const timeline = [
    message({ id: "m1", mine: true, sender: "alex" }),
    message({ id: "m2", mine: true, sender: "alex", attribution: "by claude-code for alex" }),
    message({ id: "m3", mine: true, sender: "alex", deleted: true, text: "" }),
    message({ id: "m4", mine: true, sender: "alex", shared: { item: "cv-1", kind: "artifact", title: "Q3" } }),
    message({ id: "m5" }),
  ];
  assert.equal(lastEditable(timeline)?.id, "m1");
  // What the person wrote through Chat itself is theirs.
  assert.equal(editable(message({ id: "m6", mine: true, attribution: "by chat for alex" })), true);
  assert.equal(lastEditable([message({ id: "x" })]), undefined);
});

test("cf92 one person's messages close together join; an agent's, another day's or a gap don't", () => {
  const a = message({ id: "a", at: "2026-10-20T14:00:00" });
  assert.equal(joins(undefined, a), false);
  assert.equal(joins(a, message({ id: "b", at: "2026-10-20T14:03:00" })), true);
  assert.equal(joins(a, message({ id: "b", at: "2026-10-20T14:09:00" })), false);
  assert.equal(joins(a, message({ id: "b", sender: "sam", at: "2026-10-20T14:01:00" })), false);
  assert.equal(joins(a, message({ id: "b", attribution: "by claude-code for priya", at: "2026-10-20T14:01:00" })), false);
  assert.equal(joins(message({ id: "a", at: "2026-10-19T23:59:00" }), message({ id: "b", at: "2026-10-20T00:01:00" })), false);
  assert.equal(joins(message({ id: "a", deleted: true }), message({ id: "b", at: "2026-10-20T14:01:00" })), false);
});

test("cf93 search hits group by conversation in the order they came, with the words marked", () => {
  const hit = (conversation: string, message: string): Hit => ({ conversation, title: conversation, message, thread: null, sender_name: "Priya Shah", at: "2026-10-20T14:00:00", text: "x" });
  const groups = groupHits([hit("#deal", "1"), hit("Priya Shah", "2"), hit("#deal", "3")]);
  assert.deepEqual(
    groups.map((g) => [g.title, g.hits.map((h) => h.message)]),
    [
      ["#deal", ["1", "3"]],
      ["Priya Shah", ["2"]],
    ],
  );
  assert.deepEqual(matchSegments("Northwind terms are in; north star", "north TERMS"), [
    { text: "North", match: true },
    { text: "wind ", match: false },
    { text: "terms", match: true },
    { text: " are in; ", match: false },
    { text: "north", match: true },
    { text: " star", match: false },
  ]);
  assert.deepEqual(matchSegments("abc", "  "), [{ text: "abc", match: false }]);
});

test("cf92 drafts are kept per conversation and per thread", () => {
  assert.equal(draftKey("cv-1"), "cv-1");
  assert.equal(draftKey("cv-1", null), "cv-1");
  assert.equal(draftKey("cv-1", "m-9"), "cv-1/m-9");
});

test("cf92 sidebar rows: bold and a count when unread, @ when mentioned, quiet when muted", () => {
  const row = (over: Partial<Conversation>): Conversation => ({ id: "cv", kind: "channel", title: "#deal", people: [], unread: 0, last_at: null, last_from: null, ...over });
  assert.deepEqual(rowBadges(row({})), { bold: false, count: 0, mentions: 0, muted: false });
  assert.deepEqual(rowBadges(row({ unread: 3 })), { bold: true, count: 3, mentions: 0, muted: false });
  // A mention shows as @, in place of the count.
  assert.deepEqual(rowBadges(row({ unread: 3, mentions: 1 })), { bold: true, count: 0, mentions: 1, muted: false });
  assert.deepEqual(rowBadges(row({ unread: 3, notify: "mentions" })), { bold: true, count: 0, mentions: 0, muted: false });
  assert.deepEqual(rowBadges(row({ unread: 3, mentions: 1, notify: "none" })), { bold: false, count: 0, mentions: 1, muted: true });
});
