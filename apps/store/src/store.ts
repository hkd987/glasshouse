// The Store's logic (DV-10, DV-11): what the org's catalog offers this
// person, sorted into what's theirs and what they may install, and what
// each item may do (DV-3) in plain words. Kernel patches IT reviewed for
// this computer have a category of their own: Hardware (ADR 0057).

export type Kind = "agent" | "app" | "flow" | "hardware_patch";
export type Install = "required" | "available";

export interface Declares {
  actions: string[];
  max_label: string;
  egress: string[];
}

export interface Item {
  id: string;
  kind: Kind;
  name: string;
  version: string;
  publisher: string;
  publisher_verified: boolean;
  internal: boolean;
  install: Install;
  installed: boolean;
  summary: string;
  declares: Declares;
  /** An app someone at the org made and IT approved (ADR 0032, R2). */
  app?: Made | null;
  /** A kernel patch IT reviewed, for this computer (ADR 0057). */
  patch?: Patch | null;
}

/** A kernel patch IT reviewed, and where it is on this computer. */
export interface Patch {
  patch: string;
  targets: string[];
  /** It carries a whole kernel. */
  kernel: boolean;
  bytes: number;
  reviewed_by: string;
  /** staged, probation, kept, failed, rolled_back, undone... or null: never tried here. */
  state: string | null;
}

/** Who at the org made an app, by login, and what it remixes (`mail@0.1.0`), if anything. */
export interface Made {
  made_by: string;
  based_on: string | null;
}

/** What `catalog.list` answers. */
export interface Listing {
  catalog_version: number | null;
  managed: boolean;
  items: Item[];
}

export const KIND_NAMES: Record<Kind, string> = { agent: "Agent", app: "App", flow: "Recorded flow", hardware_patch: "Kernel patch" };

/** Yours (IT installed it, or you did), what you may install, and the kernel patches for this computer. */
export function sections(items: Item[], query = ""): { yours: Item[]; available: Item[]; hardware: Item[] } {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  const matches = (item: Item) =>
    words.every((w) =>
      [item.name, item.summary, item.publisher, KIND_NAMES[item.kind]].some((text) => text.toLowerCase().includes(w)),
    );
  const shown = items.filter(matches).sort((a, b) => a.name.localeCompare(b.name));
  const apps = shown.filter((i) => i.kind !== "hardware_patch");
  return {
    yours: apps.filter((i) => i.installed),
    available: apps.filter((i) => !i.installed),
    hardware: shown.filter((i) => i.kind === "hardware_patch"),
  };
}

/** Where a kernel patch is on this computer, for a person. */
export function patchState(patch: Patch): string | null {
  switch (patch.state) {
    case "staging":
    case "staged":
      return "Restart to try it";
    case "probation":
      return "Being tried: a crash goes back";
    case "kept":
      return "This computer runs on it";
    case "failed":
    case "rolled_back":
      return "It didn't work here and went back";
    case "undone":
    case "cancelled":
      return "Taken out";
    default:
      return null;
  }
}

/**
 * Who made it: someone at the org (and what it remixes), the org, a
 * verified publisher, or neither. `items` is the listing, for the name of
 * what it remixes; failing that, its id.
 */
export function publisherLine(item: Item, items: Item[] = []): string {
  if (item.app) {
    // The catalog's publisher is the maker's name; their login otherwise.
    const by = item.publisher || item.app.made_by;
    const base = item.app.based_on?.split("@")[0];
    if (!base) return `Made by ${by} · a new app`;
    return `Made by ${by} · remix of ${items.find((i) => i.id === base)?.name ?? base}`;
  }
  if (item.internal) return `${item.publisher} · built by your organization`;
  if (item.publisher_verified) return `${item.publisher} · verified publisher`;
  return item.publisher;
}

/** What it may do, in a line (DV-3): its actions, its data, its network. */
export function canDo(declares: Declares): string {
  const actions = declares.actions.length ? declares.actions.join(", ") : "nothing on its own";
  const network = declares.egress.length ? `reaches ${declares.egress.join(", ")}` : "no network of its own";
  return `${actions} · data up to ${declares.max_label} · ${network}`;
}

/** The empty page, when there's nothing to list. */
export function emptyText(listing: Listing | null): string | null {
  if (!listing) return null;
  if (listing.catalog_version === null) {
    return listing.managed
      ? "No catalog from IT has reached this computer yet. It arrives on its own when this computer is online."
      : "This computer isn't managed by an organization, so there's no catalog to show. A public store for personal computers comes later.";
  }
  if (!listing.items.length) return "Your organization's catalog offers you nothing yet.";
  return null;
}
