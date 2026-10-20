// The Store (DV-10, DV-11): what your organization's catalog offers you.
// IT decides what's in it per group: required items are yours already,
// available ones you may install, and each says what it may do (DV-3).
// Installing changes only what you see; policy still decides what runs
// (ADR 0023). Every call goes through the bridge and the audit log.

import { Bridge, BridgeError, Offline, h, keepFilesOffline, ready, show } from "@glasshouse/sdk";
import { type Item, KIND_NAMES, type Listing, canDo, emptyText, publisherLine, sections } from "./store.ts";

const bridge = new Bridge({ app: "store" });
const state = {
  listing: null as Listing | null,
  query: "",
  busy: null as string | null,
  notice: null as { text: string; tone: "ok" | "bad" } | null,
  offline: false,
  user: "",
};

const root = document.getElementById("app") as HTMLElement;

async function load(): Promise<void> {
  try {
    const read = await bridge.read<Listing>("catalog.list", {});
    state.offline = read.offline;
    if (read.result.ok) state.listing = read.result.data;
    else state.notice = { text: read.result.text, tone: "bad" };
  } catch (error) {
    state.notice = {
      text: error instanceof Offline ? "The Store hasn't been loaded on this device yet." : String(error),
      tone: "bad",
    };
  }
  render();
}

async function change(item: Item, action: "catalog.install" | "catalog.remove"): Promise<void> {
  state.busy = item.id;
  state.notice = null;
  render();
  try {
    const result = await bridge.call(action, { id: item.id });
    state.notice = { text: result.ok ? done(item, action) : result.text, tone: result.ok ? "ok" : "bad" };
  } catch (error) {
    state.notice = {
      text:
        error instanceof Offline
          ? "That needs the system, and it isn't answering. Nothing changed."
          : error instanceof BridgeError
            ? error.message
            : String(error),
      tone: "bad",
    };
  }
  state.busy = null;
  await load();
}

function done(item: Item, action: string): string {
  return `${action === "catalog.remove" ? "Removed" : "Installed"} ${item.name}.`;
}

// ---- rendering ------------------------------------------------------------

function render(): void {
  const listing = state.listing;
  const empty = emptyText(listing);
  const { yours, available } = sections(listing?.items ?? [], state.query);
  show(
    root,
    titlebar(),
    h(
      "main",
      { class: "page" },
      state.notice && h("div", { class: `banner ${state.notice.tone === "ok" ? "done" : "refused"}`, role: "status" }, state.notice.text),
      state.offline && h("div", { class: "banner offline" }, "Offline: this is the catalog as it was last loaded. Installing needs the system."),
      empty && h("p", { class: "empty" }, empty),
      !empty && section("Available to you", available, "Nothing else to install."),
      !empty && section("Yours", yours, "Nothing installed from the catalog yet."),
    ),
  );
}

function titlebar(): HTMLElement {
  return h(
    "header",
    { class: "titlebar" },
    h("h1", {}, "Store"),
    h("span", { class: "muted" }, state.listing?.managed ? "From your organization" : ""),
    h("span", { class: "spacer" }),
    h("input", {
      class: "search",
      type: "search",
      placeholder: "Search",
      "aria-label": "Search the catalog",
      value: state.query,
      oninput: (event: Event) => {
        state.query = (event.target as HTMLInputElement).value;
        renderKeepingFocus();
      },
    }),
    state.user && h("span", { class: "user" }, state.user),
  );
}

/** Re-renders without losing the search box's caret. */
function renderKeepingFocus(): void {
  const at = (document.activeElement as HTMLInputElement | null)?.selectionStart ?? null;
  render();
  const search = root.querySelector<HTMLInputElement>("input.search");
  search?.focus();
  if (search && at !== null) search.setSelectionRange(at, at);
}

function section(title: string, items: Item[], none: string): HTMLElement {
  return h(
    "section",
    { class: "section" },
    h("h2", {}, title),
    items.length ? h("div", { class: "grid" }, ...items.map(card)) : h("p", { class: "none" }, none),
  );
}

function card(item: Item): HTMLElement {
  const busy = state.busy === item.id;
  const button =
    item.install === "required"
      ? h("span", { class: "chip" }, "Installed by IT")
      : item.installed
        ? h("button", { class: "button", disabled: busy || state.offline, onclick: () => change(item, "catalog.remove") }, busy ? "Removing…" : "Remove")
        : h("button", { class: "button primary", disabled: busy || state.offline, onclick: () => change(item, "catalog.install") }, busy ? "Installing…" : "Install");
  return h(
    "article",
    { class: "card", "data-id": item.id },
    h(
      "div",
      { class: "card-head" },
      h("div", { class: `icon ${item.kind}` }, item.name.charAt(0).toUpperCase()),
      h(
        "div",
        { class: "card-title" },
        h("strong", {}, item.name),
        h("span", { class: "muted small" }, `${KIND_NAMES[item.kind]} · v${item.version}`),
      ),
      button,
    ),
    h("p", { class: "summary" }, item.summary),
    h("p", { class: "publisher muted small" }, publisherLine(item, state.listing?.items ?? [])),
    h("p", { class: "can-do small" }, h("span", { class: "label" }, "Can do "), canDo(item.declares)),
  );
}

async function start(): Promise<void> {
  keepFilesOffline();
  try {
    const session = await bridge.open();
    state.user = session.user;
  } catch {
    // Shown below: the catalog as last loaded, or why there isn't one.
  }
  await load();
  ready();
}

void start();
