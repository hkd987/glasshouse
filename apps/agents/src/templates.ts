// A new agent comes from the ones IT approved, never a blank form (ADR
// 0065 §12): pick one, name it if you like, and it joins your computer (ADR 0068).

import { h, show } from "@glasshouse/sdk";

import { type AgentView, type Template, api } from "./api.ts";
import { app, failed, go, loadTemplates, refetch, remember, toast } from "./app.ts";
import { avatar, button, focusHeading, icon, notice, reachList } from "./ui.ts";

export async function drawNew(main: HTMLElement): Promise<void> {
  document.title = "New agent · Glasshouse Agents";
  const grid = h("div", { class: "templates" }, h("p", { class: "muted" }, "Loading the agents IT approved…"));
  const root = h(
    "div",
    { class: "page new" },
    h("div", { class: "back-row" }, h("a", { class: "back", href: "/" }, icon("back"), "All agents")),
    h(
      "div",
      {},
      h("h1", {}, "New agent"),
      h("p", { class: "sub" }, "Pick one your IT team approved. Your agents share a computer of your own, and ask you before anything leaves it."),
    ),
    grid,
    h("p", { class: "muted small" }, "Want one that isn’t here? Ask IT: they add agents to this list for your team."),
  );
  show(main, root);
  focusHeading(root);
  try {
    const templates = app.templates ?? (await loadTemplates());
    show(grid, ...(templates.length ? templates.map(templateCard) : [h("p", { class: "empty-card" }, "Your IT team hasn’t approved any agents for you yet. Ask them to add one.")]));
  } catch (error) {
    show(grid, notice((error as Error).message, "bad"));
  }
}

function templateCard(t: Template): HTMLElement {
  const nameId = `name-${t.id}`;
  const name = h("input", { id: nameId, type: "text", autocomplete: "off", maxlength: "60", placeholder: t.name });
  const add = button(`Add ${t.name}`, async () => {
    add.disabled = true;
    add.textContent = "Adding…";
    try {
      const chosen = name.value.trim();
      const view = await api<AgentView>("POST", "/agents", chosen && chosen !== t.name ? { template: t.id, name: chosen } : { template: t.id });
      remember(view);
      app.tab = "all";
      go("/");
      toast(`${view.name} is ready on your computer.`);
      void refetch().catch(() => undefined);
    } catch (error) {
      failed(error);
      add.disabled = false;
      add.textContent = `Add ${t.name}`;
    }
  }, "primary");
  return h(
    "section",
    { class: "template", "aria-labelledby": `t-${t.id}` },
    avatar(t.name),
    h("div", {}, h("h2", { id: `t-${t.id}` }, t.name), h("p", { class: "role" }, t.team)),
    h("p", { class: "does" }, t.does),
    reachList(t.reach),
    h("details", { class: "rename" }, h("summary", {}, "Give it another name"), h("label", { for: nameId }, "Its name"), name),
    h("div", { class: "push" }, add),
  );
}
