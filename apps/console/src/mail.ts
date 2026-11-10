// Mail (NV-3): the org's mail service, set once by IT so Mail on every
// device is set up with no steps. It is part of policy: saved to the draft,
// previewed, published to the pilot ring and promoted like any rule. Each
// person's own address comes from the org's directory; the address template
// stands in for people it doesn't list. Credentials never come through here.

import { h, show } from "@glasshouse/sdk";

import { type Impact, type MailPage, type MailServer, type MailSettings, api } from "./api.ts";
import { capital, card, carryOut, mono, notice, SIGNED_OUT } from "./ui.ts";

const SERVICES: [MailSettings["service"], string][] = [
  ["microsoft-365", "Microsoft 365"],
  ["google-workspace", "Google Workspace"],
  ["imap", "IMAP and SMTP"],
];

function blank(): MailSettings {
  return {
    service: "imap",
    address_template: "",
    incoming: { host: "", port: 993, security: "tls" },
    outgoing: { host: "", port: 587, security: "starttls" },
  };
}

/** What the form sends: only what the chosen service uses. */
function settings(form: MailSettings): MailSettings {
  const out: MailSettings = { service: form.service };
  if (form.address_template?.trim()) out.address_template = form.address_template.trim();
  if (form.service === "imap") {
    out.incoming = form.incoming;
    out.outgoing = form.outgoing;
  } else if (form.tenant?.trim()) {
    out.tenant = form.tenant.trim();
  }
  return out;
}

export async function mailPage(root: HTMLElement): Promise<void> {
  let page = await api<MailPage>("GET", "/api/mail");
  const form: MailSettings = { ...blank(), ...(page.draft ?? {}) };
  let impact: Impact | null = null;
  let message: { text: string; tone: "ok" | "bad" } | null = null;

  async function act(work: () => Promise<string | null>): Promise<void> {
    const said = await carryOut(work, async () => {
      page = await api<MailPage>("GET", "/api/mail");
    });
    if (said === SIGNED_OUT) return;
    message = said;
    draw();
  }

  function input(label: string, value: string, set: (v: string) => void, attrs: Record<string, string> = {}): HTMLElement {
    return h(
      "label",
      { class: "field" },
      label,
      h("input", { class: "text", value, oninput: (e: Event) => set((e.target as HTMLInputElement).value), ...attrs }),
    );
  }

  function server(name: "incoming" | "outgoing", title: string): HTMLElement {
    const s = (form[name] ??= blank()[name] as MailServer);
    return h(
      "div",
      { class: "server-row" },
      input(`${title} server`, s.host, (v) => (s.host = v), { name: `${name}-host`, placeholder: name === "incoming" ? "imap.acme.com" : "smtp.acme.com" }),
      input("Port", String(s.port), (v) => (s.port = Number(v) || 0), { name: `${name}-port`, inputmode: "numeric" }),
      h(
        "label",
        { class: "field" },
        "Security",
        h(
          "select",
          { name: `${name}-security`, onchange: (e: Event) => (s.security = (e.target as HTMLSelectElement).value as MailServer["security"]) },
          h("option", { value: "tls", selected: s.security === "tls" }, "TLS"),
          h("option", { value: "starttls", selected: s.security === "starttls" }, "STARTTLS"),
        ),
      ),
    );
  }

  function draw(): void {
    const rings = page.rings.map((r) =>
      h(
        "div",
        { class: "ring" },
        h("span", { class: "hint" }, capital(r.ring)),
        mono(r.version ? `v${r.version}` : "–"),
        h("span", { class: "hint" }, r.mail ? SERVICES.find(([id]) => id === r.mail?.service)?.[1] ?? r.mail.service : "No mail service"),
      ),
    );
    show(
      root,
      h(
        "div",
        { class: "page" },
        h(
          "header",
          { class: "page-head" },
          h(
            "div",
            {},
            h("div", { class: "crumbs" }, "Mail"),
            h("h1", {}, "Mail"),
            h("p", { class: "sub" }, "Where the org's mail is, so Mail on every computer is set up with no steps. People's own addresses come from the directory", page.people ? mono(` v${page.people}`) : null, "."),
          ),
          h(
            "div",
            { class: "actions" },
            h(
              "button",
              {
                class: "button",
                "data-test": "preview",
                onclick: () =>
                  act(async () => {
                    impact = await api<Impact>("POST", "/api/policy/preview", {});
                    return null;
                  }),
              },
              "Preview impact",
            ),
            h(
              "button",
              {
                class: "button primary",
                "data-test": "publish",
                disabled: !page.changed || !impact,
                onclick: () =>
                  act(async () => {
                    const r = await api<{ version: number }>("POST", "/api/policy/publish", { note: impact?.changes.find((c) => c.includes("mail")) ?? "Everyone: mail service", previewed: impact?.draft });
                    impact = null;
                    return `Published v${r.version} to the pilot ring. Pilot devices apply it in about a second.`;
                  }),
              },
              "Publish to pilot ring",
            ),
          ),
        ),
        message ? notice(message.text, message.tone) : null,
        h(
          "div",
          { class: "columns" },
          h(
            "div",
            { class: "main" },
            card(
              "Mail service",
              "Saved to the policy draft. Passwords and tokens never go here: each computer keeps its own.",
              h(
                "label",
                { class: "field" },
                "Service",
                h(
                  "select",
                  {
                    name: "service",
                    onchange: (e: Event) => {
                      form.service = (e.target as HTMLSelectElement).value as MailSettings["service"];
                      draw();
                    },
                  },
                  ...SERVICES.map(([id, name]) => h("option", { value: id, selected: form.service === id }, name)),
                ),
              ),
              form.service === "imap"
                ? h("div", {}, server("incoming", "Incoming (IMAP)"), server("outgoing", "Outgoing (SMTP)"))
                : input("Domain", form.tenant ?? "", (v) => (form.tenant = v), { name: "tenant", placeholder: "acme.com" }),
              input("Address for people the directory doesn't list", form.address_template ?? "", (v) => (form.address_template = v), {
                name: "address_template",
                placeholder: "{login}@acme.com",
              }),
              h(
                "div",
                { class: "actions" },
                h(
                  "button",
                  {
                    class: "button primary",
                    "data-test": "save-mail",
                    onclick: () =>
                      act(async () => {
                        await api("POST", "/api/policy/edit", { edit: "mail", mail: settings(form) });
                        impact = await api<Impact>("POST", "/api/policy/preview", {});
                        return "Saved to the draft. Preview, then publish to the pilot ring.";
                      }),
                  },
                  "Save to draft",
                ),
                page.draft
                  ? h(
                      "button",
                      {
                        class: "button",
                        onclick: () =>
                          act(async () => {
                            await api("POST", "/api/policy/edit", { edit: "mail", mail: null });
                            Object.assign(form, blank(), { tenant: undefined });
                            return "Mail service taken out of the draft.";
                          }),
                      },
                      "Remove",
                    )
                  : null,
              ),
            ),
          ),
          h(
            "div",
            { class: "side" },
            card("Versions and rollout", "Mail settings reach the pilot ring first, then early, then broad.", h("div", { class: "rings" }, ...rings)),
            impact
              ? card(
                  "Preview impact",
                  impact.base_version ? `Compared with v${impact.base_version}, which every device runs.` : null,
                  h("ul", { class: "impact", "data-test": "impact" }, ...impact.changes.map((c) => h("li", {}, c))),
                )
              : null,
          ),
        ),
      ),
    );
  }
  draw();
}
