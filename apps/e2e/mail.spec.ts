// Mail against the real system (UI-5, AU-10, DV-3, NV-3): the agent's
// draft shows who wrote it, a send waits on the desktop and the app says
// so, a decline comes back as a result, the app can call only what its
// manifest declares, and what was loaded reads offline.

import { mkdirSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import type { AddressInfo } from "node:net";
import { userInfo } from "node:os";
import { join } from "node:path";
import { type Page, expect, test } from "@playwright/test";

import { type ApprovalView, DEV, SHOTS, appUrl, audit, glassctl, glassctlJson, mcp, outcome } from "./env.ts";

async function openMail(page: Page): Promise<void> {
  await page.goto(appUrl("mail"));
  await expect(page.locator("html[data-ready=true]")).toHaveCount(1);
}

async function pendingAsk(): Promise<ApprovalView> {
  for (let i = 0; i < 100; i++) {
    const asks = glassctlJson<ApprovalView[]>("approvals");
    const ask = asks.find((a) => a.agent === "mail");
    if (ask) return ask;
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error("no ask from Mail reached the desktop");
}

test.beforeAll(() => mkdirSync(SHOTS, { recursive: true }));

test("au10 the inbox shows the reply an agent drafted, by claude-code for the user", async ({ page }) => {
  await openMail(page);
  const dana = page.locator(".row", { hasText: "Renewal terms for next year" });
  await expect(dana).toContainText("Reply drafted");
  await expect(dana.locator(".attribution")).toHaveText(/^by claude-code for \S+$/);
  await dana.click();
  await expect(page.locator("[data-test=message]")).toContainText("could we move the start to January 1");
  const card = page.locator("[data-test=draft-card]");
  await expect(card).toContainText("January 1 works for us");
  await expect(card.locator(".attribution")).toHaveText(/^by claude-code for \S+$/);
  await page.screenshot({ path: join(SHOTS, "mail-inbox-agent-draft.png") });

  // The draft itself, in Drafts, says the same.
  await page.click("[data-folder=drafts]");
  await page.locator(".row", { hasText: "Re: Renewal terms" }).click();
  await expect(page.locator("[data-test=message] .meta .attribution")).toHaveText(/^by claude-code for /);
});

test("ui5 ad1 people from recent mail are suggested as you type, and any address works", async ({ page }) => {
  // No directory on this computer: suggestions come from the mail on it.
  await openMail(page);
  await page.click("[data-test=compose]");
  const to = page.locator("input[name=to]");
  await to.pressSequentially("dan");
  const dana = page.locator(".suggestion", { hasText: "dana@northwind.example" });
  await expect(dana).toHaveCount(1);
  await expect(to).toHaveAttribute("aria-expanded", "true");
  await page.screenshot({ path: join(SHOTS, "mail-suggests-recent-people.png") });
  await to.press("Enter");
  await expect(to).toHaveValue("dana@northwind.example, ");
  await expect(page.locator(".suggestion")).toHaveCount(0);
  // Someone nobody has written to yet is kept as typed.
  await to.pressSequentially("ops@vendor.example");
  await to.press("Escape");
  await expect(to).toHaveValue("dana@northwind.example, ops@vendor.example");
  await page.locator("input[name=subject]").fill("Renewal");
  await page.locator("textarea[name=body]").fill("Two of us.");
  await page.getByRole("button", { name: "Save draft" }).click();
  await page.click("[data-folder=drafts]");
  await expect(page.locator(".row", { hasText: "Renewal" }).first()).toContainText("Renewal");
});

test("ui5 mail that changes while someone writes leaves them their message, their caret and the keyboard", async ({ page }) => {
  await openMail(page);
  await page.click("[data-test=compose]");
  await page.fill("input[name=subject]", "Notes from today");
  const body = page.locator("textarea[name=body]");
  await body.click();
  await page.keyboard.type("First line and the second");
  // Back to after "First line", as someone goes back to add a few words.
  for (let i = 0; i < " and the second".length; i++) await page.keyboard.press("ArrowLeft");
  // Something only this very element has, to tell it from one made anew.
  await body.evaluate((el) => {
    (el as HTMLTextAreaElement & { mine?: boolean }).mine = true;
  });

  // Meanwhile their agent drafts a reply. Mail changed, so the page hears
  // of it, reads its folders again and draws itself. It used to replace
  // everything it showed, the field being typed in included: the keyboard
  // went nowhere mid-sentence.
  const reread = page.waitForResponse(
    (response) => response.url().includes("/_bridge/") && (response.request().postData() ?? "").includes('"drafts"'),
  );
  const task = glassctlJson<{ id: string; driver_socket: string }>("run", "claude-code", "Draft a reply", "--yes");
  const [drafted] = (await mcp(task.driver_socket, [
    { name: "mail_draft", arguments: { to: ["ana@example.com"], subject: "While you were typing", body: "A draft." } },
  ])) as { isError?: boolean }[];
  expect(drafted?.isError ?? false).toBe(false);
  await reread;
  // Drawn: after the answers, a frame.
  await page.evaluate(() => new Promise((drawn) => requestAnimationFrame(() => requestAnimationFrame(drawn))));

  // The person is where they were: the same field, with the keyboard, the
  // caret mid-line, and all they wrote.
  const now = await page.evaluate(() => {
    const el = document.querySelector("textarea[name=body]") as HTMLTextAreaElement & { mine?: boolean };
    return { same: el.mine === true, typing: document.activeElement === el, caret: el.selectionStart, value: el.value };
  });
  expect(now).toEqual({ same: true, typing: true, caret: "First line".length, value: "First line and the second" });
  await page.keyboard.type(", a third");
  await expect(body).toHaveValue("First line, a third and the second");
  await expect(page.locator("input[name=subject]")).toHaveValue("Notes from today");
  await page.screenshot({ path: join(SHOTS, "mail-writing-while-mail-changes.png") });
  // And the page did take the change in: the draft is in Drafts.
  await page.click("[data-folder=drafts]");
  await expect(page.locator(".row", { hasText: "While you were typing" })).toHaveCount(1);
  glassctl("stop", task.id);
});

test("ui5 a send outside the team waits for approval on the desktop, then goes", async ({ page }) => {
  await openMail(page);
  await page.locator(".row", { hasText: "Renewal terms for next year" }).click();
  await page.click("[data-test=review-draft]");
  await page.click("[data-test=send-draft]");

  const ask = await pendingAsk();
  expect(ask.summary).toMatch(/Send/);
  expect(ask.items.join(" ")).toContain("dana@northwind.example");
  await expect(page.locator("[data-test=waiting]")).toContainText("Waiting for your approval on the desktop");
  await expect(page.locator("[data-test=waiting-banner]")).toContainText(ask.summary);
  await page.screenshot({ path: join(SHOTS, "mail-send-waiting-approval.png") });

  glassctl("approve", ask.id);
  await expect(page.locator("[data-test=outbox-sent]")).toContainText("Sent");
  await page.click("[data-folder=sent]");
  await expect(page.locator(".row", { hasText: "Re: Renewal terms for next year" })).toHaveCount(1);

  // Audited as the app acting for the user, approved on the desktop.
  const send = audit().filter((r) => r.event.kind === "action" && r.event.action === "mail.send" && r.event.agent === "mail");
  expect(send.map(outcome)).toContain("completed");
  expect(send.at(-1)?.event.decision?.tier).toBe("ask");
});

test("ui5 a send declined on the desktop is a result the app shows", async ({ page }) => {
  await openMail(page);
  await page.click("[data-test=compose]");
  await page.fill("input[name=to]", "marco@fabrikam.example");
  await page.fill("input[name=subject]", "Forecast questions");
  await page.fill("textarea[name=body]", "Two questions on the services line before Wednesday.");
  await expect(page.locator("[data-test=composer]")).toContainText("Sending waits for your approval on the desktop.");
  await page.click("[data-test=send]");
  const ask = await pendingAsk();
  glassctl("deny", ask.id);
  await expect(page.locator("[data-test=refused]")).toContainText("Declined on the desktop; not sent");
  expect(audit().some((r) => r.event.kind === "action" && r.event.agent === "mail" && outcome(r) === "denied")).toBe(true);
});

test("dv3 the page can call only what Mail's manifest declares, and only its own bridge", async ({ page }) => {
  await openMail(page);
  const refused = await page.evaluate(async () => {
    const response = await fetch("/_bridge/call", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ action: "calendar.create", input: {} }),
    });
    return response.json();
  });
  expect(refused.ok).toBe(false);
  expect(refused.text).toContain("doesn't declare calendar.create");
  expect(audit().some((r) => r.event.kind === "action" && r.event.agent === "mail" && r.event.action === "calendar.create" && outcome(r) === "blocked")).toBe(true);

  // Calendar's bridge is another origin: the browser won't let Mail's page call it.
  const crossOrigin = await page.evaluate(async (url) => {
    try {
      await fetch(`${url}_bridge/call`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ action: "calendar.list", input: {} }),
      });
      return "reached";
    } catch {
      return "blocked";
    }
  }, appUrl("calendar"));
  expect(crossOrigin).toBe("blocked");
});

test("ui5 what was loaded reads offline", async ({ page, context }) => {
  await openMail(page);
  await page.locator(".row", { hasText: "Wire memo for Thursday" }).click();
  await expect(page.locator("[data-test=message]")).toContainText("before 2:30 tomorrow");
  await page.evaluate(() => navigator.serviceWorker.ready);
  await context.setOffline(true);
  await page.reload();
  await expect(page.locator("html[data-ready=true]")).toHaveCount(1);
  await expect(page.locator("[data-test=offline]")).toContainText("Offline");
  await page.locator(".row", { hasText: "Wire memo for Thursday" }).click();
  await expect(page.locator("[data-test=message]")).toContainText("before 2:30 tomorrow");
  await expect(page.locator("[data-test=compose]")).toBeDisabled();
  await context.setOffline(false);
});

test("ui5 open Mail and Calendar say when the system goes away, and when it is back, with nobody clicking", async ({ page, context }) => {
  // One going-away for both: each page's wait for news is what notices,
  // and a wait under way ends only when the system would have answered it
  // (25 s at most), so the two are watched side by side.
  await openMail(page);
  const calendar = await context.newPage();
  await calendar.goto(appUrl("calendar"));
  await expect(calendar.locator("html[data-ready=true]")).toHaveCount(1);
  for (const open of [page, calendar]) {
    await open.evaluate(() => navigator.serviceWorker.ready);
    await expect(open.locator("[data-test=offline]")).toHaveCount(0);
  }

  // Nothing is clicked or reloaded.
  await context.setOffline(true);
  await Promise.all([page, calendar].map((open) => expect(open.locator("[data-test=offline]")).toContainText("Offline", { timeout: 40_000 })));
  await expect(page.locator("[data-test=compose]")).toBeDisabled();
  await expect(calendar.locator("[data-test=new-event]")).toBeDisabled();

  // Back: said within the pause between asks, not at the end of a wait.
  await context.setOffline(false);
  await Promise.all([page, calendar].map((open) => expect(open.locator("[data-test=offline]")).toHaveCount(0, { timeout: 20_000 })));
  await expect(page.locator("[data-test=compose]")).toBeEnabled();
  await expect(calendar.locator("[data-test=new-event]")).toBeEnabled();
  await calendar.close();
});

test("em0 mail is archived and rescued from junk, each a move the app makes", async ({ page }) => {
  await openMail(page);
  await page.locator(".row", { hasText: "Laptop updates tonight" }).click();
  await page.click("[data-test=move-junk]");
  await expect(page.locator(".row", { hasText: "Laptop updates tonight" })).toHaveCount(0);
  await page.click("[data-folder=junk]");
  await page.locator(".row", { hasText: "Laptop updates tonight" }).click();
  await expect(page.locator("[data-test=move-inbox]")).toHaveText("Not junk");
  await page.click("[data-test=move-inbox]");
  await page.click("[data-folder=inbox]");
  await page.locator(".row", { hasText: "Laptop updates tonight" }).click();
  await page.click("[data-test=move-archive]");
  await page.click("[data-folder=archive]");
  await expect(page.locator(".row", { hasText: "Laptop updates tonight" })).toHaveCount(1);
  // The person's own moves, in Mail: run without asking, audited.
  const moved = audit().filter((r) => r.event.kind === "action" && r.event.action === "mail.move" && r.event.agent === "mail");
  expect(moved.map(outcome)).toEqual(["completed", "completed", "completed"]);
});

test("em3 opening a message marks it read, and Mark unread brings it back", async ({ page }) => {
  // ADR 0050: what the person hasn't opened is new; Mail marks it as they open it.
  await openMail(page);
  const row = page.locator(".row", { hasText: "Q4 forecast review" });
  const count = page.locator("[data-folder=inbox] .count");
  await expect(row).toHaveClass(/unread/);
  const before = Number(await count.textContent());
  expect(before).toBeGreaterThan(0);
  const marks = () => audit().filter((r) => r.event.kind === "action" && r.event.action === "mail.mark" && r.event.agent === "mail");
  const marked = marks().length;

  await row.click();
  await expect(page.locator("[data-test=mark-unread]")).toBeVisible();
  await expect(row).not.toHaveClass(/unread/);
  await expect(count).toHaveText(before > 1 ? String(before - 1) : "");
  // It stays read when Mail loads the inbox again.
  await page.reload();
  await expect(page.locator("html[data-ready=true]")).toHaveCount(1);
  await expect(page.locator(".row", { hasText: "Q4 forecast review" })).not.toHaveClass(/unread/);

  await page.locator(".row", { hasText: "Q4 forecast review" }).click();
  await page.click("[data-test=mark-unread]");
  await expect(page.locator(".row", { hasText: "Q4 forecast review" })).toHaveClass(/unread/);
  await expect(count).toHaveText(String(before));
  // The person's own marks, in Mail: run without asking, audited.
  await expect.poll(() => marks().length).toBe(marked + 2);
  expect(marks().slice(marked).map(outcome)).toEqual(["completed", "completed"]);
});

test("nv3 an html message runs no script and loads no remote image", async ({ page }) => {
  // A server that notes whatever reaches it: a remote image, stylesheet,
  // background, script, redirect, form or link would (ADR 0037).
  const hits: string[] = [];
  const beacon = createServer((req, res) => {
    hits.push(req.url ?? "");
    res.end("x");
  });
  await new Promise<void>((resolve) => beacon.listen(0, "127.0.0.1", resolve));
  const remote = `http://127.0.0.1:${(beacon.address() as AddressInfo).port}`;
  try {
    const stamp = Math.floor(Date.now() / 1000);
    writeFileSync(
      join(DEV, "home/Mail/Inbox", `${stamp}-0-quarterly-numbers.eml`),
      [
        "From: Vendor News <news@vendor.example>",
        `To: ${userInfo().username}@corp.example`,
        "Subject: Quarterly numbers",
        `Date: ${new Date().toISOString().replace(/\.\d+Z$/, "Z")}`,
        "MIME-Version: 1.0",
        "Content-Type: text/html; charset=utf-8",
        "",
        `<html><head><meta http-equiv="refresh" content="0;url=${remote}/refresh">`,
        `<link rel="stylesheet" href="${remote}/style.css"></head>`,
        `<body style="background-image: url(${remote}/background.png)">`,
        `<h1 id="headline">Quarterly numbers are in</h1>`,
        `<img src="${remote}/pixel.gif" width="1" height="1" alt="">`,
        `<script>parent.postMessage("ran", "*"); fetch("${remote}/script");`,
        `document.getElementById("headline").textContent = "SCRIPT RAN";</script>`,
        `<form action="${remote}/form"><button id="go">Confirm</button></form>`,
        `<p><a id="link" href="${remote}/link">Read more</a></p></body></html>`,
        "",
      ].join("\r\n"),
    );
    await page.addInitScript(() => {
      const seen: unknown[] = [];
      (window as unknown as { seen: unknown[] }).seen = seen;
      window.addEventListener("message", (e) => seen.push(e.data));
    });
    await openMail(page);
    await page.locator(".row", { hasText: "Quarterly numbers" }).click();
    const frame = page.frameLocator("[data-test=html]");
    // The HTML shows, as it was sent, and its script never ran.
    await expect(frame.locator("#headline")).toHaveText("Quarterly numbers are in");
    await expect(page.locator("[data-test=html]")).toHaveAttribute("sandbox", "");
    await page.screenshot({ path: join(SHOTS, "mail-html-sandboxed.png") });
    // Its form and link go nowhere.
    await frame.locator("#go").click();
    await frame.locator("#link").click();
    await page.waitForTimeout(1500);
    await expect(frame.locator("#headline")).toHaveText("Quarterly numbers are in");
    expect(hits).toEqual([]);
    expect(await page.evaluate(() => (window as unknown as { seen: unknown[] }).seen)).toEqual([]);
  } finally {
    beacon.close();
  }
});
