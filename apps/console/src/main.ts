// The org's admin console (ADR 0023): one web page served by the console's
// own servers, where IT signs in and sets agent policy, approves catalog
// items and sees the fleet. It reads everything from the console's API,
// never from the device it's opened on. Pages: #/policy/<group>,
// #/catalog, #/catalog/<item>, #/mail, #/devices.

import { h, ready, show } from "@glasshouse/sdk";

import { Refused, type Session, SignedOut, api } from "./api.ts";
import { catalogPage, itemPage } from "./catalog.ts";
import { devicesPage } from "./devices.ts";
import { mailPage } from "./mail.ts";
import { policyPage } from "./policy.ts";
import { notice } from "./ui.ts";

const app = document.getElementById("app") as HTMLElement;
let session: Session | null = null;

const NAV: [string, string][] = [
  ["policy", "Policies"],
  ["catalog", "Catalog"],
  ["mail", "Mail"],
  ["devices", "Devices"],
];

function layout(current: string, content: HTMLElement): HTMLElement {
  return h(
    "div",
    { class: "frame" },
    h(
      "header",
      { class: "topbar" },
      h("span", { class: "logo", "aria-hidden": "true" }, "⊞"),
      h("strong", {}, "Admin console"),
      h("span", { class: "org" }, `· ${session?.org.name ?? ""}`),
      h("span", { class: "spacer" }),
      h("span", { class: "who" }, session?.display ?? ""),
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
      ...NAV.map(([id, name]) => h("a", { href: `#/${id}`, class: id === current ? "current" : "" }, name)),
    ),
    h("main", { class: "content" }, content),
  );
}

function signIn(error: SignedOut): void {
  const admins = error.admins;
  show(
    app,
    h(
      "div",
      { class: "signin" },
      h(
        "section",
        { class: "card" },
        h("h1", {}, "Admin console"),
        h("p", { class: "sub" }, error.org ? `Sign in to manage ${error.org}.` : "Sign in."),
        h("p", { class: "hint" }, "Development sign-in: pick one of the org's IT admins. On our web servers, your org's sign-in does this."),
        ...admins.map((a) =>
          h(
            "button",
            {
              class: "button wide",
              "data-user": a.user,
              onclick: async () => {
                await api("POST", "/api/session", { user: a.user });
                route();
              },
            },
            a.display,
          ),
        ),
      ),
    ),
  );
  ready();
}

async function route(): Promise<void> {
  document.documentElement.dataset.ready = "false";
  const [, section = "policy", arg] = location.hash.replace(/^#/, "").split("/");
  const content = h("div", { class: "loading" });
  try {
    session ??= await api<Session>("GET", "/api/session");
    show(app, layout(section, content));
    if (section === "catalog" && arg) await itemPage(content, decodeURIComponent(arg));
    else if (section === "catalog") await catalogPage(content);
    else if (section === "devices") await devicesPage(content);
    else if (section === "mail") await mailPage(content);
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
