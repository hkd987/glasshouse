// The Store's logic (DV-10, DV-11): what the org's catalog offers this
// person, sorted into what's theirs and what they may install, and what
// each item may do (DV-3) in plain words.

export type Kind = "agent" | "app" | "flow";
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
}

/** What `catalog.list` answers. */
export interface Listing {
  catalog_version: number | null;
  managed: boolean;
  items: Item[];
}

export const KIND_NAMES: Record<Kind, string> = { agent: "Agent", app: "App", flow: "Recorded flow" };

/** Yours (IT installed it, or you did), then what you may install. */
export function sections(items: Item[], query = ""): { yours: Item[]; available: Item[] } {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  const matches = (item: Item) =>
    words.every((w) =>
      [item.name, item.summary, item.publisher, KIND_NAMES[item.kind]].some((text) => text.toLowerCase().includes(w)),
    );
  const shown = items.filter(matches).sort((a, b) => a.name.localeCompare(b.name));
  return {
    yours: shown.filter((i) => i.installed),
    available: shown.filter((i) => !i.installed),
  };
}

/** Who made it: built by the org, a verified publisher, or neither. */
export function publisherLine(item: Item): string {
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
