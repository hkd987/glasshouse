// The console's admin API (crates/bin/glass-console/src/server.rs): JSON
// over the console's own origin, with the session cookie. A refusal is an
// answer with `error`, shown to IT as it is.

export class SignedOut extends Error {
  readonly org: string;
  readonly admins: { user: string; display: string }[];
  constructor(org: string, admins: { user: string; display: string }[]) {
    super("sign in");
    this.org = org;
    this.admins = admins;
  }
}

export class Refused extends Error {}

export async function api<T>(method: "GET" | "POST" | "DELETE", path: string, body?: unknown): Promise<T> {
  const response = await fetch(path, {
    method,
    headers: body === undefined ? {} : { "content-type": "application/json" },
    body: body === undefined ? null : JSON.stringify(body),
    credentials: "same-origin",
  });
  const data = (await response.json().catch(() => ({}))) as Record<string, unknown>;
  if (response.status === 401) {
    throw new SignedOut(String(data.org ?? ""), (data.admins as { user: string; display: string }[]) ?? []);
  }
  if (!response.ok) throw new Refused(String(data.error ?? `The console answered ${response.status}`));
  return data as T;
}

export type Tier = "auto" | "notify" | "ask" | "never";
export type Ring = "pilot" | "early" | "broad";
export type Mode = "required" | "available" | "blocked";

export interface Session {
  user: string;
  display: string;
  org: { id: string; name: string };
}

export interface GroupRef {
  name: string;
  display: string;
  source: string;
  users: number;
}

export interface Live {
  group: string;
  running: number;
  waiting: number;
  blocked_24h: number;
  rollbacks_7d: number;
}

export interface PolicyPage {
  group: GroupRef;
  groups: GroupRef[];
  in_policy: boolean;
  draft_version: number;
  draft_changed: boolean;
  classes: { id: string; label: string; tier: Tier | null; inherited: boolean; actions: string[] }[];
  agents: {
    id: string;
    name: string;
    version: string | null;
    allowed: boolean;
    everyone: boolean;
    actions: string[];
    egress: string[];
  }[];
  apps: string[];
  max_label: string | null;
  org_max_label: string;
  routes: { label: string; route: string }[];
  versions: { version: number; note: string; by: string; at: string; rolls_back_to: number | null; rings: Ring[] }[];
  rings: Partial<Record<Ring, number>>;
  live: Live;
}

export interface Impact {
  base_version: number | null;
  users: number;
  agents: number;
  agent_ids: string[];
  groups: string[];
  org_wide: boolean;
  changes: string[];
  warnings: string[];
}

export interface Item {
  id: string;
  kind: string;
  kind_name: string;
  name: string;
  version: string;
  publisher: string;
  verified: boolean;
  internal: boolean;
  summary: string;
  review: "waiting" | "approved" | "blocked";
  requested_by: string;
  decided_by: string | null;
  checks: string[];
  pinned: boolean;
  groups: { group: string; display: string; users: number; mode: Mode }[];
  declares: { actions: { action: string; title: string; tier: Tier | null }[]; max_label: string; egress: string[] };
  public_store: string | null;
}

export interface CatalogPage {
  version: number | null;
  counts: { waiting: number; approved: number; blocked: number };
  groups: GroupRef[];
  items: Item[];
}

export interface DeviceRow {
  id: string;
  name: string;
  ring: Ring;
  online: boolean;
  last_seen: string;
  users: string[];
  os_version: string | null;
  policy_version: number | null;
  catalog_version: number | null;
  agents: number;
  waiting: number;
  blocked_24h: number;
  last_error: string | null;
}

export interface FleetPage {
  totals: { devices: number; online: number; running: number; waiting: number; blocked_24h: number; users: number };
  rings: { ring: Ring; devices: number; policy_version: number | null }[];
  catalog_version: number | null;
  devices: DeviceRow[];
  enroll: { url: string; org: string; org_key: string; admins: number };
}
