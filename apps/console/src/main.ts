// The org's admin console (ADR 0023): one web page served by the console's
// own servers, where IT signs in and sets agent policy, approves catalog
// items and sees the fleet. It reads everything from the console's API,
// never from the device it's opened on. Pages: #/policy/<group>,
// #/catalog, #/catalog/<item>, #/settings/<group>, #/mail, #/credits,
// #/remote-help, #/devices for IT's
// admins; #/support for support staff (ADR 0028). The pages someone sees
// follow from their session; the console refuses the rest anyway.

import { h, ready, show } from "@glasshouse/sdk";

import { Refused, type Session, SignedOut, api } from "./api.ts";
import { catalogPage, itemPage } from "./catalog.ts";
import { devicesPage } from "./devices.ts";
import { creditsPage } from "./credits.ts";
import { mailPage } from "./mail.ts";
import { policyPage } from "./policy.ts";
import { rolesPage } from "./roles.ts";
import { settingsPage } from "./settings.ts";
import { supportPage } from "./support.ts";
import { notice } from "./ui.ts";

const app = document.getElementById("app") as HTMLElement;
let session: Session | null = null;

const ADMIN_NAV: [string, string][] = [
  ["policy", "Policies"],
  ["catalog", "Catalog"],
  ["settings", "Settings"],
  ["mail", "Mail"],
  ["credits", "Model credits"],
  ["remote-help", "Remote help"],
  ["devices", "Devices"],
];

/** The pages this person's session opens, in the order the sidebar shows them. */
function nav(): [string, string][] {
  return [...(session?.admin ? ADMIN_NAV : []), ...(session?.roles.length ? ([["support", "Support"]] as [string, string][]) : [])];
}

function layout(current: string, content: HTMLElement): HTMLElement {
  return h(
    "div",
    { class: "frame" },
    h(
      "header",
      { class: "topbar" },
      h("span", { class: "logo", "aria-hidden": "true" }, "⊞"),
      h("strong", {}, session?.admin ? "Admin console" : "IT support"),
      h("span", { class: "org" }, `· ${session?.org.name ?? ""}`),
      h("span", { class: "spacer" }),
      h("span", { class: "who" }, [session?.display, ...(session?.roles.map((r) => r.name) ?? [])].filter(Boolean).join(" · ")),
      h(
        "button",
        {
          class: "signout",
          onclick: async () => {
            await api("DELETE", "/api/session");
            session = null;
            route();
          },
        },
        "Sign out",
      ),
    ),
    h(
      "nav",
      { class: "sidebar", "aria-label": "Sections" },
      ...nav().map(([id, name]) => h("a", { href: `#/${id}`, class: id === current ? "current" : "" }, name)),
    ),
    h("main", { class: "content" }, content),
  );
}

function signIn(error: SignedOut): void {
  show(
    app,
    h(
      "div",
      { class: "signin" },
      h(
        "section",
        { class: "card" },
        h("h1", {}, "Admin console"),
        h("p", { class: "sub" }, error.org ? `Sign in to ${error.org}'s console.` : "Sign in."),
        h("p", { class: "hint" }, "Development sign-in: pick one of the org's IT admins or support staff. On our web servers, your org's sign-in does this."),
        ...error.people.map((a) =>
          h(
            "button",
            {
              class: "button wide signer",
              "data-user": a.user,
              onclick: async () => {
                await api("POST", "/api/session", { user: a.user });
                route();
              },
            },
            h("span", {}, a.display),
            h("span", { class: "hint" }, a.title),
          ),
        ),
      ),
    ),
  );
  ready();
}

async function route(): Promise<void> {
  document.documentElement.dataset.ready = "false";
  const [, asked, arg] = location.hash.replace(/^#/, "").split("/");
  const content = h("div", { class: "loading" });
  let section = asked ?? "";
  try {
    session ??= await api<Session>("GET", "/api/session");
    // A page this session doesn't open goes to the first one it does.
    const pages = nav().map(([id]) => id);
    if (!pages.includes(section)) section = pages[0] ?? "";
    show(app, layout(section, content));
    if (section === "support") await supportPage(content);
    else if (section === "remote-help") await rolesPage(content);
    else if (section === "settings") await settingsPage(content, arg ? decodeURIComponent(arg) : "everyone");
    else if (section === "catalog" && arg) await itemPage(content, decodeURIComponent(arg));
    else if (section === "catalog") await catalogPage(content);
    else if (section === "devices") await devicesPage(content);
    else if (section === "mail") await mailPage(content);
    else if (section === "credits") await creditsPage(content);
    else await policyPage(content, arg ? decodeURIComponent(arg) : "ap-ops");
    document.documentElement.dataset.page = section;
    performance.mark(`console:${section}`);
    ready();
  } catch (error) {
    if (error instanceof SignedOut) {
      session = null;
      signIn(error);
    } else if (error instanceof Refused && arg && section === "policy") {
      location.hash = "#/policy/everyone";
    } else {
      show(content, notice((error as Error).message, "bad"));
      ready();
    }
  }
}

window.addEventListener("hashchange", () => void route());
void route();
