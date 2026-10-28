// The page's state and what it does with the front: what it loaded, and
// the answers, asks and computer controls every screen shares. Screens
// draw from `app` and call these; main.ts routes and redraws.

import { h, show } from "@glasshouse/sdk";

import {
  type AgentDetail,
  type AgentView,
  type ApprovalView,
  type PendingApproval,
  type Session,
  type Template,
  type UndoResult,
  agentPath,
  api,
} from "./api.ts";
import type { Tab } from "./model.ts";
import { announce, button, clock, icon } from "./ui.ts";

export const app = {
  session: null as Session | null,
  agents: [] as AgentView[],
  approvals: [] as PendingApproval[],
  templates: null as Template[] | null,
  details: new Map<string, AgentDetail>(),
  tab: "all" as Tab,
  /** Confidential questions the person chose to see. */
  revealed: new Set<string>(),
  /** Questions already announced, so each is said once. */
  heard: new Set<string>(),
  /** Set by main.ts: redraws what shows. */
  redraw: (): void => {},
};

export function agent(id: string): AgentView | undefined {
  return app.agents.find((a) => a.id === id) ?? app.details.get(id)?.agent;
}

/** Keeps an agent's card in step with an answer that carries it. */
export function remember(view: AgentView): void {
  const at = app.agents.findIndex((a) => a.id === view.id);
  if (at >= 0) app.agents[at] = view;
  else app.agents.push(view);
  const detail = app.details.get(view.id);
  if (detail) detail.agent = view;
}

export async function loadAgents(): Promise<void> {
  const [agents, approvals] = await Promise.all([api<AgentView[]>("GET", "/agents"), api<PendingApproval[]>("GET", "/approvals")]);
  app.agents = agents;
  app.approvals = approvals;
  for (const p of approvals) {
    if (!app.heard.has(p.approval.id) && loadedOnce) announce(`${p.agent_name} needs you.`);
    app.heard.add(p.approval.id);
  }
  loadedOnce = true;
}

let loadedOnce = false;

export async function loadDetail(id: string): Promise<AgentDetail> {
  const detail = await api<AgentDetail>("GET", agentPath(id));
  app.details.set(id, detail);
  remember(detail.agent);
  return detail;
}

export async function loadTemplates(): Promise<Template[]> {
  app.templates = await api<Template[]>("GET", "/templates");
  return app.templates;
}

/** Opens a page of this app, without loading it again. */
export function go(path: string): void {
  if (`${location.pathname}${location.search}` !== path) history.pushState(null, "", path);
  window.dispatchEvent(new PopStateEvent("popstate"));
}

// --- The toast: what just happened, or why not. -------------------------

let toastTimer = 0;

export function toast(text: string, tone: "ok" | "bad" = "ok"): void {
  const box = document.getElementById("toast");
  if (!box) return;
  window.clearTimeout(toastTimer);
  show(
    box,
    h(
      "div",
      { class: `toast ${tone}`, role: tone === "bad" ? "alert" : "status" },
      h("span", {}, text),
      button("Dismiss", () => clearToast(), "link small", { "aria-label": "Dismiss this message" }),
    ),
  );
  if (tone === "ok") toastTimer = window.setTimeout(clearToast, 10_000);
}

export function clearToast(): void {
  window.clearTimeout(toastTimer);
  document.getElementById("toast")?.replaceChildren();
}

export function failed(error: unknown): void {
  toast((error as Error).message || "Something went wrong. Try again in a moment.", "bad");
}

// --- Actions every screen shares. ----------------------------------------

/** Answers a question, from the home screen or the conversation. */
export async function answer(agentId: string, agentName: string, approval: ApprovalView, yes: boolean): Promise<boolean> {
  try {
    await api<ApprovalView>("POST", `/approvals/${encodeURIComponent(approval.id)}`, { yes, via: "web" });
    toast(yes ? `Approved: ${approval.yes_label}. ${agentName} carries on.` : `Not done. ${agentName} won't do it, and it's on the record.`);
    await refetch(agentId);
    return true;
  } catch (error) {
    failed(error);
    await refetch(agentId).catch(() => undefined);
    return false;
  }
}

/** Reloads the cards and questions, and the agent's own page when it's
 * loaded, then redraws. */
export async function refetch(agentId?: string): Promise<void> {
  await Promise.all([loadAgents(), agentId && app.details.has(agentId) ? loadDetail(agentId) : null]);
  app.redraw();
}

export type Control = "take-over" | "hand-back" | "wake" | "sleep";

const CONTROL_DONE: Record<Control, (name: string) => string> = {
  "take-over": (name) => `You have ${name}'s computer. ${name} waits until you hand it back.`,
  "hand-back": (name) => `Handed back. ${name} carries on.`,
  wake: (name) => `${name} is waking up. It picks up where it left off.`,
  sleep: (name) => `${name} is going to sleep. Its computer is saved to storage.`,
};

/** Takes over, hands back, wakes or puts to sleep. */
export async function control(id: string, what: Control, quiet = false): Promise<AgentView | null> {
  try {
    const view = await api<AgentView>("POST", agentPath(id, `/${what}`));
    remember(view);
    if (!quiet) toast(CONTROL_DONE[what](view.name));
    app.redraw();
    void refetch(id).catch(() => undefined);
    return view;
  } catch (error) {
    failed(error);
    return null;
  }
}

/** Undo, asked first: a button that turns into "Put back …? Undo it /
 * Keep it". `since` is when the task started. */
export function undoControl(id: string, since: string, label: string, cls = ""): HTMLElement {
  const box = h("span", { class: "undo-box" });
  const start = (): void => {
    show(box, button([icon("undo"), label], ask, `secondary ${cls}`, { "data-key": `undo-${id}` }));
  };
  const ask = (): void => {
    const keep = button("Keep it", () => {
      start();
      box.querySelector("button")?.focus();
    }, "secondary");
    show(
      box,
      h(
        "span",
        { class: "undo-ask", role: "group", "aria-label": "Undo this task?" },
        h("span", {}, `Put its computer and any file of yours it changed back as they were at ${clock(since)}?`),
        h("span", { class: "row" }, button("Undo it", go, "danger"), keep),
      ),
    );
    keep.focus();
  };
  const go = async (): Promise<void> => {
    show(box, h("span", { class: "muted" }, "Undoing…"));
    try {
      const result = await api<UndoResult>("POST", agentPath(id, "/undo"));
      const left = result.left?.length ? ` Couldn't put back: ${result.left.join("; ")}.` : "";
      toast(`${result.text}${left}`);
      await refetch(id);
    } catch (error) {
      failed(error);
      start();
    }
  };
  start();
  return box;
}
