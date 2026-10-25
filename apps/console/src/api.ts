// The console's admin API (crates/bin/glass-console/src/server.rs): JSON
// over the console's own origin, with the session cookie. A refusal is an
// answer with `error`, shown to IT as it is.

/** Someone the development sign-in offers: an admin or support staff. */
export interface Signer {
  user: string;
  display: string;
  /** "IT admin", their support roles' names, or both. */
  title: string;
}

export class SignedOut extends Error {
  readonly org: string;
  readonly people: Signer[];
  constructor(org: string, people: Signer[]) {
    super("sign in");
    this.org = org;
    this.people = people;
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
    throw new SignedOut(String(data.org ?? ""), (data.people as Signer[]) ?? []);
  }
  if (!response.ok) throw new Refused(String(data.error ?? `The console answered ${response.status}`));
  return data as T;
}

export type Tier = "auto" | "notify" | "ask" | "never";
export type Ring = "pilot" | "early" | "broad";
export type Mode = "required" | "available" | "blocked";

/** Remote help's tools (CF-81), a closed list. */
export type SupportTool = "view" | "control" | "install_apps" | "collect_logs" | "restart_services" | "isolate" | "forensics";

export interface Session {
  user: string;
  display: string;
  org: { id: string; name: string };
  /** One of IT's admins: every page but Support. */
  admin: boolean;
  /** The support roles their groups hold: the Support page. */
  roles: { id: string; name: string; tools: SupportTool[] }[];
  /** The org's model credits running low or used up, for an admin (ADR 0039). */
  credits?: "low" | "used_up" | null;
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
  /** The draft this preview shows: publishing names it (AD-3, ADR 0053). */
  draft: string;
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
  /** A shared app's version computers keep while a newer one waits (ADR 0032, R2). */
  approved_version?: string | null;
  /** An app someone at the org made and shared, for IT's review (GA-8). */
  shared?: Shared | null;
  /** What a shared app changes from its base, file by file: on the item's own page only. */
  changes?: FileChange[];
  /** A kernel patch an agent wrote on one of the org's computers, for IT's review (ADR 0057). */
  hardware?: Hardware | null;
}

/** A kernel patch sent for review (ADR 0057, slice 3): who asked on which computer, its source and its build here. */
export interface Hardware {
  person: string;
  person_login: string;
  device: string;
  computer: string;
  asked: string;
  patch: string;
  release: string;
  targets: string[];
  modules: string[];
  kernel: boolean;
  /** `agent:<task>`. */
  origin: string;
  diff: string;
  build: { state: "building" } | { state: "built"; log: string; bytes: number } | { state: "failed"; log: string };
  submitted_at: string;
  /** Of the reviewed bundle, once approved. */
  bundle_sha256: string | null;
  /** Computers whose reports say they keep it, are trying it, or went back. */
  computers: { kept: number; trying: number; went_back: number };
}

/** An action, as IT reads it. */
export interface Titled {
  action: string;
  title: string;
}

/** A shared app's review (ADR 0032, R2): who made it from what, and the console's own checks. */
export interface Shared {
  /** Their display name, and their login. */
  maker: string;
  maker_login: string;
  /** The version of their own app they shared. */
  maker_version: number;
  device: string;
  /** For their team (a directory group), or everyone in the organization. */
  to: "team" | "org";
  group: string | null;
  group_name: string | null;
  note: string;
  /** What it remixes; none for a new app. */
  based_on: { id: string; version: string } | null;
  /** What the computer's own checks noted. */
  device_notes: string[];
  checks: { ok: boolean; text: string }[];
  passed: boolean;
  /** The privilege delta: every action for a new app. */
  beyond_base: Titled[];
  /** What it declares that can't be undone. */
  irreversible: Titled[];
  /** DV-4: a person must review it; approving it is that review. */
  needs_person: boolean;
  submitted_at: string;
  bundle_sha256: string | null;
}

/** One file a shared app changes. `unified`: `@@ -a,b +c,d @@` hunks of ` `, `-` and `+` lines. */
export interface FileChange {
  path: string;
  status: "added" | "removed" | "changed";
  added: number;
  removed: number;
  unified: string;
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
  /** What kind of computer IT says it is, for unattended remote help (ADR 0028). */
  kind: string | null;
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
  /** Whether it signs with a key of its own (ADR 0030 slice P). */
  registered: boolean;
  /** When IT removed it from the organization: refused since. */
  removed: string | null;
}

/** An app someone made on a computer (GA-10, ADR 0032). */
export interface MadeApp {
  app: string;
  name: string;
  owner: string;
  device: string;
  version: number;
  based_on: string | null;
  actions: string[];
  beyond_base: string[];
  saved_at: string;
}

export interface FleetPage {
  /** The kinds a device can be given: those support roles reach unattended. */
  kinds: string[];
  totals: { devices: number; online: number; running: number; waiting: number; blocked_24h: number; users: number };
  rings: { ring: Ring; devices: number; policy_version: number | null }[];
  catalog_version: number | null;
  devices: DeviceRow[];
  made: MadeApp[];
  enroll: { url: string; org: string; org_key: string; admins: number };
}

/** The org's mail service (NV-3), as policy carries it. */
export interface MailServer {
  host: string;
  port: number;
  security: "tls" | "starttls";
}

export interface MailSettings {
  service: "imap" | "microsoft-365" | "google-workspace";
  address_template?: string;
  tenant?: string;
  incoming?: MailServer;
  outgoing?: MailServer;
}

/** The Model credits page (ADR 0039): never the key. */
export interface CreditsPage {
  /** This console sells credits. */
  offered: boolean;
  /** The policy draft has credits on for agents. */
  on: boolean;
  /** What the pilot ring runs has them on. */
  live?: boolean;
  /** Running low or used up, by the alert below. */
  standing: "low" | "used_up" | null;
  /** Admins are warned below this many dollars left. */
  alert_below_usd: number;
  /** An admin chose it; otherwise a tenth of what was bought, at least $5. */
  alert_set: boolean;
  granted_usd: number;
  spent_usd: number | null;
  remaining_usd: number | null;
  grants: { at: string; usd: number; note: string; paid: boolean }[];
  /** An admin accepted the model providers' terms for the org. */
  terms_accepted: boolean;
  /** What each computer spent, most first: never what was said. */
  by_computer: { device: string; calls: number; spent_usd: number; last: string | null }[];
  /** Admins may buy credits here, by card. */
  can_buy: boolean;
  min_usd: number;
  max_usd: number;
  /** Why usage couldn't be read, if it couldn't. */
  problem: string | null;
}

export interface MailPage {
  draft: MailSettings | null;
  draft_version: number;
  /** The draft's mail service isn't what the pilot ring runs. */
  changed: boolean;
  rings: { ring: Ring; version: number | null; mail: MailSettings | null }[];
  /** The directory release devices have, if one was signed. */
  people: number | null;
}

/** The Support page (CF-80): the person's roles, every tool, and the computers they reach. */
export interface SupportPage {
  roles: { id: string; name: string; tools: SupportTool[]; people: string[]; unattended: string[]; max_minutes: number }[];
  tools: { id: SupportTool; title: string; mine: boolean; roles: string[]; ready: boolean }[];
  devices: { id: string; name: string; kind: string | null; unattended: boolean; online: boolean; last_seen: string; users: string[]; os_version: string | null }[];
  answer_within_secs: number;
  policy_version: number | null;
}

/** A support role as policy carries it (ADR 0028). */
export interface SupportRole {
  id: string;
  name: string;
  groups: string[];
  tools: SupportTool[];
  people: string[];
  unattended: string[];
  max_minutes: number;
}

/** The Remote help page (CF-81): the draft's roles and who holds them. */
export interface RolesPage {
  answer_within_secs: number;
  roles: (SupportRole & { holders: string[]; holder_count: number })[];
  /** The draft's roles aren't what the pilot ring runs. */
  changed: boolean;
  pilot_version: number | null;
  tools: { id: SupportTool; title: string }[];
  groups: { name: string; display: string }[];
}

/** A device setting's value (UX-1). */
export type SettingValue = boolean | number | string | string[];

/** A setting as IT set it for a group: a default people can change, or locked. */
export interface Managed {
  value: SettingValue;
  locked?: boolean;
}

export interface SettingRow {
  id: string;
  section: string;
  title: string;
  help: string;
  kind: { type: "toggle" } | { type: "number"; min: number; max: number; unit: string } | { type: "url" } | { type: "sites" } | { type: "time_zone" } | { type: "apps" } | { type: "folders" } | { type: "networks" };
  /** A person's own, or the whole computer's (set for everyone only). */
  scope: "person" | "device" | "seat";
  default: SettingValue;
  /** What this group sets in the draft. */
  set: Managed | null;
  /** What everyone gets, for a group that doesn't set it. */
  everyone: Managed | null;
  /** What this group's pilot computers run. */
  pilot: Managed | null;
}

/** The Settings page (UX-1). */
export interface SettingsPage {
  group: { name: string; display: string; people: number };
  groups: { name: string; display: string; people: number; sets: number }[];
  /** The policy's groups, higher first: who wins when groups disagree. */
  order: string[];
  settings: SettingRow[];
  changed: boolean;
  pilot_version: number | null;
}
