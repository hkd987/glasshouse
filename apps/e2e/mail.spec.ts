// Mail against the real system (UI-5, AU-10, DV-3, NV-3): the agent's
// draft shows who wrote it, a send waits on the desktop and the app says
// so, a decline comes back as a result, the app can call only what its
// manifest declares, and what was loaded reads offline.

import { mkdirSync } from "node:fs";
import { join } from "node:path";
import { type Page, expect, test } from "@playwright/test";

import { type ApprovalView, SHOTS, appUrl, audit, glassctl, glassctlJson, outcome } from "./env.ts";

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
  const send = audit().filter((r) => r.event.action === "mail.send" && r.event.agent === "mail");
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
  expect(audit().some((r) => r.event.agent === "mail" && outcome(r) === "denied")).toBe(true);
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
  expect(audit().some((r) => r.event.agent === "mail" && r.event.action === "calendar.create" && outcome(r) === "blocked")).toBe(true);

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
