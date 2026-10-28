// Glasshouse Agents, the web app (ADR 0065 §12, ADR 0066): every
// employee's agents, all on a computer of their own, served by the front
// (`glass-agents`) at an address in the org's domain. Pages, by path (the
// front serves this page at each): / (home), /new, /agents/<id> (its
// conversation; ?approval=<id> opens at that question, as Teams' Open
// does) and /agents/<id>/computer (its screen). Live updates come from a long poll of /api/v1/events; what it
// names is refetched. No websockets, no storage of its own.

import { h, ready, show } from "@glasshouse/sdk";

import { AgentScreen } from "./agent.ts";
import { type Events, type Session, SignedOut, api } from "./api.ts";
import { app, go, loadAgents, loadDetail } from "./app.ts";
import { ComputerScreen } from "./computer.ts";
import { type Change, EventLoop, sleep } from "./events.ts";
import { drawHome } from "./home.ts";
import { type Route, initials, parseRoute, waitingLabel } from "./model.ts";
import { drawNew } from "./templates.ts";
import { button, focusHeading, icon, keepFocus, notice } from "./ui.ts";

const root = document.getElementById("app") as HTMLElement;
const header = h("header", { class: "top" });
const banner = h("div", { class: "banner-box" });
const toastBox = h("div", { id: "toast", class: "toast-box" });
const main = h("main", { id: "main" });

let route: Route = { page: "home" };
let screen: AgentScreen | ComputerScreen | null = null;

function drawHeader(): void {
  const waiting = waitingLabel(app.approvals);
  const person = app.session?.person;
  keepFocus(header, () =>
    show(
      header,
      h("a", { class: "brand", href: "/", "aria-label": "Glasshouse Agents, home" }, icon("logo", "logo"), h("strong", {}, "Glasshouse"), h("span", {}, "Agents")),
      h(
        "div",
        { class: "top-right" },
        waiting
          ? button(
              [h("span", { class: "pill-dot", "aria-hidden": "true" }), waiting],
              () => {
                app.tab = "needs";
                if (route.page === "home") {
                  app.redraw();
                  document.getElementById("waiting-title")?.focus();
                } else go("/");
              },
              "pill",
              { "data-key": "pill", "aria-label": `${waiting}: show what's waiting` },
            )
          : null,
        app.session ? h("span", { class: "org" }, app.session.org) : null,
        person ? h("span", { class: "me", title: person.name }, h("span", { "aria-hidden": "true" }, initials(person.name)), h("span", { class: "visually-hidden" }, `Signed in as ${person.name}`)) : null,
      ),
    ),
  );
  const heading = document.getElementById("waiting-title");
  if (heading) heading.tabIndex = -1;
}

/** Redraws what shows from what's loaded. */
app.redraw = () => {
  drawHeader();
  if (route.page === "home") {
    document.title = "Your agents · Glasshouse Agents";
    drawHome(main);
  } else screen?.draw();
  const waiting = app.approvals.length;
  document.title = `${waiting ? `(${waiting}) ` : ""}${document.title.replace(/^\(\d+\) /, "")}`;
};

async function navigate(): Promise<void> {
  screen?.destroy();
  screen = null;
  route = parseRoute(location.pathname);
  window.scrollTo(0, 0);
  document.documentElement.classList.toggle("dark", route.page === "computer");
  switch (route.page) {
    case "home":
      app.redraw();
      focusHeading(main);
      break;
    case "new":
      drawHeader();
      await drawNew(main);
      break;
    case "agent": {
      const s = new AgentScreen(main, route.id);
      screen = s;
      drawHeader();
      // Teams' Open, and the home screen's: at that question.
      const approval = new URLSearchParams(location.search).get("approval");
      await s.open(approval);
      if (approval) history.replaceState(null, "", location.pathname);
      break;
    }
    case "computer": {
      const s = new ComputerScreen(main, route.id);
      screen = s;
      drawHeader();
      await s.open();
      break;
    }
    case "missing":
      drawHeader();
      show(main, h("div", { class: "page" }, h("h1", {}, "Nothing here"), h("p", {}, "This address doesn’t lead to one of your agents."), h("a", { class: "btn primary", href: "/" }, "See your agents")));
      focusHeading(main);
      break;
  }
}

/** Refetches what the events stream says changed. */
async function refresh(change: Change): Promise<void> {
  const ids = change.kind === "agents" ? change.ids : null;
  const current = route.page === "agent" || route.page === "computer" ? route.id : null;
  const touched = current !== null && (ids === null || ids.includes(current));
  await Promise.all([loadAgents(), touched && current ? loadDetail(current) : null]);
  if (touched && screen instanceof ComputerScreen) screen.poke();
  app.redraw();
}

const loop = new EventLoop({
  poll: (since, signal) => api<Events>("GET", `/events?since=${since}`, undefined, AbortSignal.any([signal, AbortSignal.timeout(40_000)])),
  changed: refresh,
  online: (yes) =>
    show(banner, yes ? null : h("p", { class: "offline", role: "status" }, "Can’t reach Glasshouse Agents right now. Trying again…")),
  fatal: (error) => {
    if (!(error instanceof SignedOut)) return false;
    signedOut(error.message);
    return true;
  },
  sleep,
  now: () => Date.now(),
});

/** Signed out: the org's sign-in (in front of the front) brings them
 * back; the front's own words say how where there's none. */
function signedOut(words: string): void {
  loop.stop();
  screen?.destroy();
  screen = null;
  show(
    root,
    h(
      "main",
      { class: "signin" },
      h(
        "section",
        { class: "panel" },
        h("span", { class: "signin-logo" }, icon("logo", "logo")),
        h("h1", {}, "You’re signed out"),
        h("p", {}, words),
        button("Sign in again", () => location.reload(), "primary big"),
      ),
    ),
  );
  document.title = "Signed out · Glasshouse Agents";
  focusHeading(root);
  ready();
}

async function boot(): Promise<void> {
  show(root, header, banner, toastBox, main);
  try {
    app.session = await api<Session>("GET", "/session");
    await loadAgents();
  } catch (error) {
    if (error instanceof SignedOut) return signedOut(error.message);
    drawHeader();
    show(main, h("div", { class: "page" }, notice((error as Error).message, "bad"), button("Try again", () => location.reload(), "primary")));
    ready();
    // Tries again by itself, so it comes back when the front does.
    await sleep(10_000, new AbortController().signal);
    return boot();
  }
  window.addEventListener("popstate", () => void navigate());
  // Links within the app open without loading the page again.
  document.addEventListener("click", (event) => {
    const link = (event.target as Element | null)?.closest?.("a[href]");
    if (!(link instanceof HTMLAnchorElement) || event.defaultPrevented || event.button !== 0) return;
    if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey || link.target || link.origin !== location.origin) return;
    if (link.pathname.startsWith("/api/")) return;
    event.preventDefault();
    go(`${link.pathname}${link.search}`);
  });
  await navigate();
  ready();
  void loop.run();
}

void boot();
