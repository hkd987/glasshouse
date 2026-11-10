// Remixing a built-in app (TL-5, GA-3, GA-4, ADR 0032): the person's own
// Claude Code finds Mail to start from, reads its source, changes it and
// saves their own version. glassd checks and bundles it; the remix opens on
// its own origin, served from what glassd built, beside the Mail everyone
// has. What the checks refuse comes back as words the agent can act on,
// and going back is choosing an earlier version.

import { mkdirSync } from "node:fs";
import { join } from "node:path";
import { type Page, expect, test } from "@playwright/test";

import { SHOTS, appUrl, audit, glassctl, glassctlJson, mcp, outcome } from "./env.ts";

interface ToolResult {
  isError?: boolean;
  content: { text: string }[];
  structuredContent?: Record<string, unknown>;
}

const text = (r: ToolResult) => r.content.map((c) => c.text).join("\n");

/** One call to the studio's tools, as Claude Code makes it. */
async function tool(socket: string, name: string, args: unknown): Promise<ToolResult> {
  const [result] = (await mcp(socket, [{ name, arguments: args }])) as ToolResult[];
  return result!;
}

async function ready(page: Page, app: string): Promise<void> {
  await page.goto(appUrl(app));
  await expect(page.locator("html[data-ready=true]")).toHaveCount(1);
}

test.beforeAll(() => mkdirSync(SHOTS, { recursive: true }));

test("tl5 ga3 ga4 claude code remixes mail into the person's own, checked, versioned, on its own origin", async ({ page }) => {
  const task = glassctlJson<{ id: string; driver_socket: string }>(
    "run",
    "claude-code",
    "Make my Mail say Write instead of New message",
    "--title",
    "My own Mail",
    "--yes",
  );
  const socket = task.driver_socket;

  // Store first, then what can be remixed (GA-12): Mail is the closest.
  const bases = await tool(socket, "apps_bases", { need: "read and write mail" });
  expect(bases.isError ?? false).toBe(false);
  expect(text(bases)).toContain("Mail can be remixed");

  const source = await tool(socket, "apps_source", { app: "mail" });
  const data = source.structuredContent as { manifest: Record<string, unknown>; files: Record<string, string> };
  const main = data.files["src/main.ts"]!;
  expect(main).toContain('"New message",');
  const files = {
    ...data.files,
    "src/main.ts": main.replace('"New message",', '"Write",').replace('h("span", {}, f.title),', 'h("span", {}, f.title.toUpperCase()),'),
  };

  // GA-4: what the checks refuse, the agent reads and fixes.
  const markup = await tool(socket, "apps_save", {
    base: "mail",
    manifest: data.manifest,
    files: { ...files, "src/banner.ts": "export const banner = (n: Element) => { n.innerHTML = '<b>Mine</b>'; };\n" },
    note: "A banner",
  });
  expect(markup.isError).toBe(true);
  expect(text(markup)).toContain("src/banner.ts:1: builds markup from a string");
  const library = await tool(socket, "apps_save", {
    base: "mail",
    manifest: data.manifest,
    files: { ...files, "src/main.ts": `import pad from "left-pad";\nconsole.log(pad("1", 3));\n${files["src/main.ts"]}` },
    note: "Padding",
  });
  expect(library.isError).toBe(true);
  expect(text(library)).toContain("src/main.ts:1: imports left-pad; an app made here builds on the Glasshouse SDK and its own files only");

  const saved = await tool(socket, "apps_save", {
    base: "mail",
    manifest: data.manifest,
    files,
    note: "Write instead of New message; folder names in capitals",
  });
  expect(saved.isError ?? false, text(saved)).toBe(false);
  expect(saved.structuredContent?.app).toBe("my-mail");
  expect(text(saved)).toContain("Saved My Mail version 1, your version of Mail");
  // ADR 0049: tried before the agent is told it's saved, in a stand-in
  // page with sample data. Mail as it ships must open there: it draws with
  // the SDK, and what the SDK needs of a page the stand-in has to have.
  expect(text(saved)).toContain("Tried here, with sample data");
  expect(text(saved)).not.toContain("couldn't be tried");
  expect(text(saved)).not.toContain("Problems to fix");
  expect(text(saved)).toContain("Opening it showed:");
  expect(text(saved)).toContain("[Write]");
  // Notify: saving their own version asks nothing, and it's on the record.
  const save = audit().find((r) => r.event.kind === "action" && r.event.action === "apps.save" && outcome(r) === "completed");
  expect(save?.event.decision?.tier).toBe("notify");

  // In the launcher's list, and on its own origin, built from the remix.
  const profile = glassctlJson<{ id: string; name: string }[]>("apps");
  expect(profile.find((a) => a.id === "my-mail")?.name).toBe("My Mail");
  await ready(page, "my-mail");
  await expect(page).toHaveTitle("My Mail");
  await expect(page.locator("[data-test=compose]")).toHaveText("Write");
  await expect(page.locator("[data-folder=inbox]")).toContainText("INBOX");
  await page.screenshot({ path: join(SHOTS, "studio-my-mail.png") });
  // The Mail everyone has is untouched.
  const theirs = await page.context().newPage();
  await ready(theirs, "mail");
  await expect(theirs.locator("[data-test=compose]")).toHaveText("New message");
  await theirs.close();

  // GA-3: a second version, then back to the first.
  const second = await tool(socket, "apps_save", {
    app: "my-mail",
    manifest: data.manifest,
    files: { ...files, "src/main.ts": files["src/main.ts"]!.replace('"Write",', '"Compose",') },
    note: "Compose, not Write",
  });
  expect(second.structuredContent?.version, text(second)).toBe(2);
  await ready(page, "my-mail");
  await expect(page.locator("[data-test=compose]")).toHaveText("Compose");
  const back = await tool(socket, "apps_revert", { app: "my-mail", version: 1 });
  expect(text(back)).toContain("My Mail is back to version 1");
  await ready(page, "my-mail");
  await expect(page.locator("[data-test=compose]")).toHaveText("Write");

  glassctl("stop", task.id);
});

test("tl5 a name that isn't an app of the person's own is no app at all", async ({ page }) => {
  const response = await page.goto(appUrl("my-nothing"));
  expect(response?.status()).toBe(421);
});
