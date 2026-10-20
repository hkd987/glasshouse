// Chat (CF-91, ADR 0030) on the console suite's enrolled computer, with
// Priya on another computer, scripted with the same store, sealing and
// console client glassd uses (crates/apps/chat/examples/colleague.rs).
// The tests tagged @c4a (CF-92, CF-93: ADR 0030 C4a, like Slack inside the
// seal) need glassd's threads, reactions, edits, pins, topics, mentions,
// drafts, levels and search, and the colleague's `call` and `conversation`;
// `--grep @c4a` runs them. They build on the four before them.

import { execFileSync } from "node:child_process";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { userInfo } from "node:os";
import { join } from "node:path";
import { type Browser, type Page, expect, test } from "@playwright/test";

import { CHAT_URL, CONSOLE_SHOTS, CONSOLE_STATE } from "./console-env.ts";
import { type ApprovalView, BIN, DEV, audit, glassctl, glassctlJson, mcp } from "./env.ts";

const me = userInfo().username;
const HELLO = "Hi Priya, the Northwind terms are in";
const ANSWER = "Thanks! Reading them now.";
const AGENT = "I'll review the terms by Friday.";

/** Priya, in Chat on her own computer. */
function priya(...args: string[]): string {
  return execFileSync(
    join(BIN, "examples/colleague"),
    ["--enrollment", join(CONSOLE_STATE, "enrollment.json"), "--state", join(DEV, "../colleague"), "--as", "priya", ...args],
    { encoding: "utf8" },
  ).trim();
}

async function openChat(browser: Browser): Promise<{ page: Page; ms: number; close: () => Promise<void> }> {
  const context = await browser.newContext({ viewport: { width: 1280, height: 800 } });
  const page = await context.newPage();
  const started = Date.now();
  await page.goto(CHAT_URL);
  await page.waitForSelector("html[data-ready=true]", { state: "attached" });
  return { page, ms: Date.now() - started, close: () => context.close() };
}

/** Every file under `dir`, for looking for text that mustn't be there. */
function everything(dir: string): string {
  return readdirSync(dir)
    .map((name) => join(dir, name))
    .map((path) => (statSync(path).isDirectory() ? everything(path) : readFileSync(path, "latin1")))
    .join("\n");
}

test("cf91 two people message each other in Chat, live and sealed end to end", async ({ browser }) => {
  priya("open");
  const { page, ms, close } = await openChat(browser);
  expect(ms, "UI-7: Chat drawn with its data in under 1 s").toBeLessThan(1000);
  await page.getByRole("button", { name: "New message" }).first().click();
  await page.locator("[name=to]").fill("Pri");
  await page.locator("[data-login=priya]").click();
  await page.locator("[name=message]").fill(HELLO);
  await page.keyboard.press("Enter");
  await expect(page.locator(".pane-head h1")).toHaveText("Priya Shah");
  await expect(page.locator("[data-test=messages]")).toContainText(HELLO);
  // What the person typed in Chat posted without asking.
  expect(glassctlJson<ApprovalView[]>("approvals")).toHaveLength(0);

  // Priya's computer checks and opens it.
  expect(priya("read", me)).toContain(HELLO);

  // Her answer shows without a reload, from an app.status wait: one read
  // per change, not one per poll.
  const reads = audit().filter((r) => r.event.action === "chat.read").length;
  priya("say", me, ANSWER);
  const said = Date.now();
  await expect(page.locator("[data-test=messages]")).toContainText(ANSWER, { timeout: 5000 });
  const shown = Date.now() - said;
  console.log(`chat: a colleague's message on screen ${shown} ms after it was posted`);
  expect(shown, "CF-91: posted on one computer, on screen on another (loopback)").toBeLessThan(1000);
  // Read where it is open: no unread count on it.
  await expect(page.locator(".row.active .badge")).toHaveCount(0);
  await page.waitForTimeout(500);
  expect(audit().filter((r) => r.event.action === "chat.read").length - reads).toBeLessThanOrEqual(2);
  await page.screenshot({ path: join(CONSOLE_SHOTS, "chat-two-people.png") });

  // GT-3: the console holds neither message.
  const held = everything(CONSOLE_STATE);
  expect(held).not.toContain("Northwind terms");
  expect(held).not.toContain("Reading them now");
  await close();
});

test("cf91 au10 an agent's post waits on the desktop and shows its by-line", async ({ browser }) => {
  const { page, close } = await openChat(browser);
  await expect(page.locator(".pane-head h1")).toHaveText("Priya Shah");
  const task = glassctlJson<{ id: string; driver_socket: string }>(
    "run",
    "claude-code",
    "Tell Priya I'll review the terms by Friday",
    "--title",
    "Reply to Priya",
    "--yes",
  );
  const posting = mcp(task.driver_socket, [{ name: "chat_post", arguments: { posts: [{ to: ["priya"], text: AGENT }] } }]);
  let ask: ApprovalView | undefined;
  await expect
    .poll(() => (ask = glassctlJson<ApprovalView[]>("approvals").find((a) => a.summary === "Message Priya Shah")), { timeout: 10_000 })
    .toBeTruthy();
  expect(ask?.items[0]).toBe(`Priya Shah: ${AGENT}`);
  // Nothing went out while it waits.
  expect(priya("read", me)).not.toContain(AGENT);
  glassctl("approve", ask!.id);
  const [result] = (await posting) as { isError?: boolean; content: { text: string }[] }[];
  glassctl("stop", task.id);
  expect(result?.isError ?? false, result?.content.map((c) => c.text).join("\n")).toBe(false);

  const message = page.locator(".message", { hasText: AGENT });
  await expect(message.locator(".attribution")).toHaveText(`by claude-code for ${me}`, { timeout: 5000 });
  expect(priya("read", me)).toContain(`"attribution":"by claude-code for ${me}"`);
  const post = audit().find((r) => r.event.kind === "action" && r.event.action === "chat.post" && r.event.agent === "claude-code");
  expect(post?.event.decision?.tier).toBe("ask");
  expect(JSON.stringify(post)).not.toContain(AGENT);
  await page.screenshot({ path: join(CONSOLE_SHOTS, "chat-agent-by-line.png") });
  await close();
});

test("cf91 a channel made in Chat with its people, who read it on their computer", async ({ browser }) => {
  const { page, close } = await openChat(browser);
  await page.getByRole("button", { name: "New channel" }).click();
  await page.locator("[name=channel]").fill("northwind-deal");
  await page.locator("[name=people]").fill("Pri");
  await page.locator("[data-login=priya]").click();
  await page.getByRole("button", { name: "Make channel" }).click();
  await expect(page.locator(".pane-head h1")).toHaveText("#northwind-deal");
  await page.locator("[name=message]").fill("Kickoff at 3 in the big room");
  await page.keyboard.press("Enter");
  await expect(page.locator("[data-test=messages]")).toContainText("Kickoff at 3");
  await expect(page.locator(".sidebar .row.active")).toContainText("northwind-deal");
  expect(priya("read", "#northwind-deal")).toContain("Kickoff at 3 in the big room");
  // The console keeps the channel's name sealed (ADR 0030).
  expect(everything(CONSOLE_STATE)).not.toContain("northwind-deal");
  await page.screenshot({ path: join(CONSOLE_SHOTS, "chat-channel.png") });
  await close();
});

test("cf91 Chat keeps nothing in the page's storage", async ({ browser }) => {
  const { page, close } = await openChat(browser);
  // Both conversations shown, then what the page kept is looked at.
  await expect(page.locator("[data-test=messages]")).toContainText("Kickoff at 3");
  await page.locator(".sidebar .row", { hasText: "Priya Shah" }).click();
  await expect(page.locator("[data-test=messages]")).toContainText(HELLO);
  const stored = await page.evaluate(async () => {
    const parts: unknown[] = [{ ...localStorage }, { ...sessionStorage }, await indexedDB.databases()];
    for (const name of await caches.keys()) {
      const cache = await caches.open(name);
      for (const request of await cache.keys()) parts.push(await (await cache.match(request))?.text());
    }
    return JSON.stringify(parts);
  });
  expect(stored).not.toContain("Northwind");
  expect(stored).not.toContain("Friday");
  expect(stored).not.toContain("Kickoff");
  await close();
});

// --- Like Slack, inside the seal (CF-92, CF-93, ADR 0030 C4a) -------------------------

const CHANNEL = "#northwind-deal";
const KICKOFF = "Kickoff at 3 in the big room";
const MY_REPLY = "I'll bring the printouts";
const HER_REPLY = "Great, I'll book the projector";
const TOPIC = "Close the Northwind renewal by Friday";
const HER_TOPIC = "Northwind renewal: pricing first, then the timeline";
const DRAFT = "Half-written thought about the pricing tiers";

interface Viewed {
  id: string;
  text: string;
  mine: boolean;
  edited: boolean;
  deleted: boolean;
  pinned: boolean;
  thread: string | null;
  replies: number;
  reactions: { emoji: string; count: number; mine: boolean; names: string[] }[];
  mentions: { login: string; name: string }[];
}

interface Read {
  conversation: { id: string; topic: string | null; pinned: string[] };
  messages: Viewed[];
}

/** Any chat action, as Priya on her computer: its data (the colleague's `call`). */
function priyaCall<T>(action: string, input: unknown): T {
  return JSON.parse(priya("call", action, JSON.stringify(input))) as T;
}

function priyaRead(conversation: string, thread?: string): Read {
  return priyaCall<Read>("chat.read", thread ? { conversation, thread } : { conversation });
}

/** Priya posts in a conversation (or a thread); the new message's id. */
function priyaPost(conversation: string, text: string, replyTo?: string): string {
  const post = priyaCall<{ posted: { id: string }[] }>("chat.post", { posts: [{ conversation, text, ...(replyTo ? { reply_to: replyTo } : {}) }] });
  const id = post.posted[0]?.id;
  if (!id) throw new Error(`Priya's post came back without an id: ${JSON.stringify(post)}`);
  return id;
}

/** Chat open on #northwind-deal. */
async function openChannel(browser: Browser): Promise<{ page: Page; close: () => Promise<void> }> {
  const { page, close } = await openChat(browser);
  const row = page.locator(".sidebar .row", { hasText: CHANNEL.slice(1) });
  if (!((await row.getAttribute("class")) ?? "").includes("active")) await row.click();
  await expect(page.locator(".pane-head h1")).toHaveText(CHANNEL);
  await expect(page.locator("[data-test=messages]")).toContainText(KICKOFF);
  return { page, close };
}

/** A message in the timeline (not the thread panel), by its text or id. */
function inTimeline(page: Page, what: { text: string } | { id: string }): ReturnType<Page["locator"]> {
  return "id" in what
    ? page.locator(`[data-test=messages] .message[data-message="${what.id}"]`)
    : page.locator("[data-test=messages] .message[data-message]", { hasText: what.text });
}

test.describe("C4a: like Slack, inside the seal", { tag: "@c4a" }, () => {
  test("cf92 a reply in a thread reaches Priya's computer, and hers shows in the panel live", async ({ browser }) => {
    const channel = priya("conversation", CHANNEL);
    const { page, close } = await openChannel(browser);
    const kickoff = inTimeline(page, { text: KICKOFF });
    const root = await kickoff.getAttribute("data-message");
    expect(root).toBeTruthy();

    await kickoff.hover();
    await kickoff.locator("[data-act=reply]").click();
    const thread = page.locator("[data-test=thread]");
    await expect(thread).toContainText(KICKOFF);
    await page.locator("[name=reply]").fill(MY_REPLY);
    await page.keyboard.press("Enter");
    await expect(thread.locator(".message[data-message]", { hasText: MY_REPLY })).toBeVisible();
    // Posted with reply_to set to the root, so it lands in the thread on her computer.
    const hers = priyaRead(channel, root!);
    expect(hers.messages.map((m) => m.text)).toEqual([KICKOFF, MY_REPLY]);
    expect(hers.messages[1]?.thread).toBe(root);
    // Replies stay out of the timeline, which shows roots only.
    expect(priyaRead(channel).messages.map((m) => m.text)).not.toContain(MY_REPLY);

    // Her reply shows in the open thread without a reload, and the root's summary counts both.
    priyaPost(channel, HER_REPLY, root!);
    await expect(thread).toContainText(HER_REPLY, { timeout: 5000 });
    await expect(kickoff.locator("[data-test=thread-summary]")).toContainText("2 replies");
    await expect(kickoff.locator("[data-test=thread-summary] .avatar")).toHaveCount(2);
    await expect(page.locator("[data-test=messages]")).not.toContainText(HER_REPLY);
    await page.screenshot({ path: join(CONSOLE_SHOTS, "chat-thread.png") });

    // Esc closes the panel (UX-20).
    await page.keyboard.press("Escape");
    await expect(thread).toBeHidden();
    await close();
  });

  test("cf92 reactions toggle per person and emoji, and each side sees the other's live", async ({ browser }) => {
    const channel = priya("conversation", CHANNEL);
    const { page, close } = await openChannel(browser);
    const kickoff = inTimeline(page, { text: KICKOFF });
    const root = (await kickoff.getAttribute("data-message"))!;

    await kickoff.hover();
    await kickoff.locator("[data-act=react]").click();
    const picker = page.locator("[data-test=emoji-picker]");
    await expect(picker.locator(".emoji-row [data-emoji]")).toHaveCount(8);
    await picker.locator("[data-test=more-emoji]").click();
    await expect(picker.locator(".emoji-grid [data-emoji]")).toHaveCount(40);
    await page.screenshot({ path: join(CONSOLE_SHOTS, "chat-emoji-picker.png") });
    await picker.locator("[data-emoji='🎉']").click();
    await expect(picker).toBeHidden();
    const party = kickoff.locator(".reaction[data-emoji='🎉']");
    await expect(party.locator(".reaction-count")).toHaveText("1");
    await expect(party).toHaveClass(/mine/);
    const onHers = () => priyaRead(channel).messages.find((m) => m.id === root)?.reactions.find((r) => r.emoji === "🎉");
    await expect.poll(() => onHers()?.count, { timeout: 5000 }).toBe(1);
    expect(onHers()?.mine).toBe(false);

    // Priya reacts too: the count shows live, and hovering names who reacted.
    priyaCall("chat.react", { conversation: channel, message: root, emoji: "🎉", on: true });
    priyaCall("chat.react", { conversation: channel, message: root, emoji: "👀", on: true });
    await expect(party.locator(".reaction-count")).toHaveText("2", { timeout: 5000 });
    await expect(kickoff.locator(".reaction[data-emoji='👀'] .reaction-count")).toHaveText("1");
    await expect(kickoff.locator(".reaction[data-emoji='👀']")).not.toHaveClass(/mine/);
    await party.hover();
    await expect(party.locator(".tip")).toHaveText("Priya Shah and you reacted with 🎉");
    await page.screenshot({ path: join(CONSOLE_SHOTS, "chat-reactions.png") });

    // Clicking the pill takes the person's back; hers stays.
    await party.click();
    await expect(party.locator(".reaction-count")).toHaveText("1");
    await expect(party).not.toHaveClass(/mine/);
    await expect.poll(() => onHers(), { timeout: 5000 }).toMatchObject({ count: 1, mine: true });
    // And she takes hers back: the pill goes.
    priyaCall("chat.react", { conversation: channel, message: root, emoji: "👀", on: false });
    await expect(kickoff.locator(".reaction[data-emoji='👀']")).toHaveCount(0, { timeout: 5000 });
    await close();
  });

  test("cf92 edits and deletes: hers show live, and the person's own (Up to edit, delete asks first) reach her", async ({ browser }) => {
    const channel = priya("conversation", CHANNEL);
    const { page, close } = await openChannel(browser);

    const lunchId = priyaPost(channel, "Lunch at 12?");
    const lunch = inTimeline(page, { id: lunchId });
    await expect(lunch).toContainText("Lunch at 12?", { timeout: 5000 });
    // Someone else's message has no edit or delete for the person.
    await expect(lunch.locator("[data-act=pin]")).toHaveCount(1);
    await expect(lunch.locator("[data-act=edit], [data-act=delete]")).toHaveCount(0);
    priyaCall("chat.edit", { conversation: channel, message: lunchId, text: "Lunch at 1?" });
    await expect(lunch.locator(".message-text")).toHaveText("Lunch at 1? (edited)", { timeout: 5000 });
    priyaCall("chat.delete", { conversation: channel, message: lunchId });
    await expect(lunch).toContainText("This message was deleted.", { timeout: 5000 });
    await expect(lunch).not.toContainText("Lunch");

    const message = page.locator("[name=message]");
    await message.fill("The agenda is in teh deck");
    await message.press("Enter");
    const mine = inTimeline(page, { text: "The agenda is in teh deck" });
    await expect(mine).toBeVisible();
    const mineId = (await mine.getAttribute("data-message"))!;
    // Up in the empty composer edits the person's last message.
    await message.press("ArrowUp");
    const edit = page.locator("[name=edit]");
    await expect(edit).toHaveValue("The agenda is in teh deck");
    await expect(edit).toBeFocused();
    await page.screenshot({ path: join(CONSOLE_SHOTS, "chat-edit.png") });
    await edit.fill("The agenda is in the deck");
    await edit.press("Enter");
    const fixed = inTimeline(page, { id: mineId });
    await expect(fixed.locator(".message-text")).toHaveText("The agenda is in the deck (edited)");
    await expect(message).toBeFocused();
    await expect
      .poll(() => priyaRead(channel).messages.find((m) => m.id === mineId), { timeout: 5000 })
      .toMatchObject({ text: "The agenda is in the deck", edited: true });

    // Delete is from the toolbar and asks first; Esc keeps the message.
    await fixed.hover();
    await fixed.locator("[data-act=delete]").click();
    const dialog = page.locator("[data-test=delete-dialog]");
    await expect(dialog).toContainText("can't be undone");
    await page.keyboard.press("Escape");
    await expect(dialog).toBeHidden();
    await expect(fixed).toContainText("The agenda is in the deck");
    await fixed.hover();
    await fixed.locator("[data-act=delete]").click();
    await page.screenshot({ path: join(CONSOLE_SHOTS, "chat-delete-confirm.png") });
    await dialog.locator("[data-test=confirm-delete]").click();
    await expect(fixed).toContainText("This message was deleted.");
    await expect.poll(() => priyaRead(channel).messages.find((m) => m.id === mineId)?.deleted, { timeout: 5000 }).toBe(true);
    expect(priyaRead(channel).messages.find((m) => m.id === mineId)?.text).toBe("");
    await close();
  });

  test("cf92 pins and the topic: set on one computer, seen on the other, live", async ({ browser }) => {
    const channel = priya("conversation", CHANNEL);
    const { page, close } = await openChannel(browser);
    const kickoff = inTimeline(page, { text: KICKOFF });
    const root = (await kickoff.getAttribute("data-message"))!;

    await kickoff.hover();
    await kickoff.locator("[data-act=pin]").click();
    await expect(page.locator("[data-test=pins-count]")).toHaveText("1");
    await expect(kickoff.locator(".pin-note")).toHaveText("Pinned");
    await expect.poll(() => priyaRead(channel).conversation.pinned, { timeout: 5000 }).toEqual([root]);

    const agenda = priyaPost(channel, "Agenda: pricing, then the timeline");
    await expect(inTimeline(page, { id: agenda })).toBeVisible({ timeout: 5000 });
    priyaCall("chat.pin", { conversation: channel, message: agenda, pinned: true });
    await expect(page.locator("[data-test=pins-count]")).toHaveText("2", { timeout: 5000 });
    await page.locator("[data-test=pins-button]").click();
    const pins = page.locator("[data-test=pins] [data-pin]");
    await expect(pins).toHaveCount(2);
    // Most recently pinned first.
    await expect(pins.first()).toContainText("Agenda: pricing");
    await page.screenshot({ path: join(CONSOLE_SHOTS, "chat-pins.png") });
    await page.locator(`[data-test=pins] [data-pin="${root}"] [data-act=unpin]`).click();
    await expect(page.locator("[data-test=pins-count]")).toHaveText("1");
    await expect.poll(() => priyaRead(channel).conversation.pinned, { timeout: 5000 }).toEqual([agenda]);
    await page.keyboard.press("Escape");
    await expect(page.locator("[data-test=pins]")).toBeHidden();

    // The topic, edited inline in the header.
    await page.locator("[data-test=topic]").click();
    const topic = page.locator("[name=topic]");
    await expect(topic).toBeFocused();
    await topic.fill(TOPIC);
    await page.screenshot({ path: join(CONSOLE_SHOTS, "chat-topic-edit.png") });
    await topic.press("Enter");
    await expect(page.locator("[data-test=topic]")).toHaveText(TOPIC);
    await expect.poll(() => priyaRead(channel).conversation.topic, { timeout: 5000 }).toBe(TOPIC);
    priyaCall("chat.topic", { conversation: channel, topic: HER_TOPIC });
    await expect(page.locator("[data-test=topic]")).toHaveText(HER_TOPIC, { timeout: 5000 });
    // Changes to a message aren't new messages: nothing is unread.
    await expect(page.locator(".sidebar .badge")).toHaveCount(0);
    await close();
  });

  test("cf92 a mention: @ suggests the people in it, and Priya's mention badges the row and shows as a chip", async ({ browser }) => {
    const channel = priya("conversation", CHANNEL);
    const { page, close } = await openChannel(browser);
    // Elsewhere, so the channel isn't read as she writes.
    await page.locator(".sidebar .row", { hasText: "Priya Shah" }).click();
    await expect(page.locator(".pane-head h1")).toHaveText("Priya Shah");

    priyaPost(channel, `@${me} can you check the numbers before 3?`);
    const row = page.locator(".sidebar .row", { hasText: CHANNEL.slice(1) });
    await expect(row.locator("[data-test=mention-badge]")).toHaveText("@1", { timeout: 5000 });
    await expect(row).toHaveClass(/unread/);
    await page.screenshot({ path: join(CONSOLE_SHOTS, "chat-mention-badge.png") });

    await row.click();
    const said = inTimeline(page, { text: "can you check the numbers" });
    await expect(said.locator(`[data-mention-chip="${me}"]`)).toBeVisible();
    await expect(said).toHaveClass(/mentioned/);
    // The New messages divider sits on the first message that was unread.
    await expect(page.locator("[data-test=new-messages] + .message")).toContainText("can you check the numbers");
    await expect(row.locator(".badge")).toHaveCount(0);

    const message = page.locator("[name=message]");
    await message.pressSequentially("Sure @Pri");
    const suggestion = page.locator("[data-mention=priya]");
    await expect(suggestion).toBeVisible();
    await expect(page.locator(`[data-mention="${me}"]`)).toHaveCount(0);
    await page.screenshot({ path: join(CONSOLE_SHOTS, "chat-mention-autocomplete.png") });
    await message.press("Enter");
    await expect(message).toHaveValue("Sure @priya ");
    await message.pressSequentially("on it");
    await message.press("Enter");
    const sent = inTimeline(page, { text: "on it" });
    await expect(sent.locator("[data-mention-chip=priya]")).toHaveText("@Priya Shah");
    await expect
      .poll(() => priyaRead(channel).messages.find((m) => m.text === "Sure @priya on it")?.mentions.map((p) => p.login), { timeout: 5000 })
      .toEqual(["priya"]);
    await close();
  });

  test("cf93 search finds messages across conversations, grouped, and opens where each one is", async ({ browser }) => {
    const dm = priya("conversation", me);
    priyaPost(dm, "The numbers for Q3 are in the shared folder");
    const { page, close } = await openChannel(browser);
    const search = page.locator("[name=search]");
    await search.fill("numbers");
    await search.press("Enter");
    const results = page.locator("[data-test=search-results]");
    await expect(results.locator(".hit-group")).toHaveCount(2);
    await expect(results.locator(".hit-group h3")).toHaveText([/Priya Shah/, /#northwind-deal/]);
    await expect(results.locator("mark").first()).toHaveText(/numbers/i);
    await page.screenshot({ path: join(CONSOLE_SHOTS, "chat-search.png") });

    // A message in a channel: opened there and pointed at.
    await results.locator(".hit-group", { hasText: CHANNEL }).locator("[data-hit]").click();
    await expect(page.locator(".pane-head h1")).toHaveText(CHANNEL);
    await expect(page.locator("[data-test=messages] .message.highlight")).toContainText("can you check the numbers");
    await page.screenshot({ path: join(CONSOLE_SHOTS, "chat-search-hit.png") });

    // A reply: its thread opens with it pointed at.
    await search.fill("printouts");
    await search.press("Enter");
    await expect(results.locator("[data-hit]")).toHaveCount(1);
    await expect(results.locator(".hit-thread")).toHaveText("in a thread");
    await results.locator("[data-hit]").click();
    await expect(page.locator("[data-test=thread] .message.highlight")).toContainText(MY_REPLY);

    // Deleted words aren't found.
    await search.fill("Lunch");
    await search.press("Enter");
    await expect(results).toContainText("Nothing found");
    // Esc leaves the search for where the person was.
    await search.press("Escape");
    await expect(page.locator(".pane-head h1")).toHaveText(CHANNEL);
    // Neither the query nor what it found is in the audit log (ADR 0030 C4a).
    const searched = audit().filter((r) => r.event.action === "chat.search");
    expect(searched.length).toBeGreaterThan(0);
    expect(JSON.stringify(searched)).not.toContain("printouts");
    expect(JSON.stringify(searched)).not.toContain("shared folder");
    await close();
  });

  test("cf92 a draft and a level are kept in glassd: they come back after Chat is closed, and the page keeps neither", async ({ browser }) => {
    const first = await openChannel(browser);
    await first.page.locator("[name=message]").fill(DRAFT);
    // Leaving the conversation keeps it at once; the row says so.
    await first.page.locator(".sidebar .row", { hasText: "Priya Shah" }).click();
    const row = first.page.locator(".sidebar .row", { hasText: CHANNEL.slice(1) });
    await expect(row.locator("[data-test=draft]")).toHaveText("Draft");
    await expect(first.page.locator("[name=message]")).toHaveValue("");
    // A thread's draft is its own.
    await row.click();
    await expect(first.page.locator("[name=message]")).toHaveValue(DRAFT);
    const kickoff = inTimeline(first.page, { text: KICKOFF });
    await kickoff.locator("[data-test=thread-summary]").click();
    await first.page.locator("[name=reply]").fill("Thread draft: ask about the room");
    await first.page.keyboard.press("Escape");

    // The level: mute the channel from the header.
    await first.page.locator("[data-test=notify-button]").click();
    const menu = first.page.locator("[data-test=notify-menu]");
    await expect(menu.locator("[aria-checked=true]")).toHaveAttribute("data-level", "mentions");
    await first.page.screenshot({ path: join(CONSOLE_SHOTS, "chat-notify-menu.png") });
    await menu.locator("[data-level=none]").click();
    await expect(first.page.locator("[data-test=notify-button]")).toHaveAttribute("data-level", "none");
    await expect(row).toHaveClass(/muted/);
    await first.close();

    // Chat closed and opened again: glassd still has both drafts and the level.
    const { page, close } = await openChannel(browser);
    await expect(page.locator("[name=message]")).toHaveValue(DRAFT);
    await expect(page.locator(".sidebar .row.active")).toHaveClass(/muted/);
    await expect(page.locator("[data-test=notify-button]")).toHaveAttribute("data-level", "none");
    await inTimeline(page, { text: KICKOFF }).locator("[data-test=thread-summary]").click();
    await expect(page.locator("[name=reply]")).toHaveValue("Thread draft: ask about the room");
    await page.screenshot({ path: join(CONSOLE_SHOTS, "chat-overview.png") });

    // Sent, a draft is gone.
    await page.locator("[name=message]").fill("Pricing tiers are final");
    await page.locator("[name=message]").press("Enter");
    await expect(inTimeline(page, { text: "Pricing tiers are final" })).toBeVisible();

    // Nothing of it is in the page's storage, nor in the audit log.
    const stored = await page.evaluate(async () => {
      const parts: unknown[] = [{ ...localStorage }, { ...sessionStorage }, await indexedDB.databases()];
      for (const name of await caches.keys()) {
        const cache = await caches.open(name);
        for (const request of await cache.keys()) parts.push(await (await cache.match(request))?.text());
      }
      return JSON.stringify(parts);
    });
    for (const words of [DRAFT, "Thread draft", MY_REPLY, HER_TOPIC, "numbers"]) expect(stored).not.toContain(words);
    const kept = audit().filter((r) => r.event.action === "chat.draft");
    expect(kept.length).toBeGreaterThan(0);
    expect(JSON.stringify(kept)).not.toContain("pricing tiers");
    await close();

    const again = await openChannel(browser);
    await expect(again.page.locator("[name=message]")).toHaveValue("");
    await again.close();
  });
});
