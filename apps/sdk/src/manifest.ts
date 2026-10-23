// What an app declares and what the bridge says, as glassd defines them
// (crates/lib/api/src/apps.rs, ADR 0024). Keep the two in step.

export type Tier = "auto" | "notify" | "ask" | "never";
export type SideEffect = "none" | "local_reversible" | "irreversible";
export type Label = "public" | "internal" | "confidential" | "restricted";
/**
 * Powers beyond actions. `offline`: keep what was loaded and show it (UI-5).
 * `media`: camera, microphone and WebRTC, for meetings only (CF-21).
 * `speech`: the person's own speech as text on the computer, for a
 * transcribed meeting (ADR 0045): its worker may run WebAssembly.
 */
export type AppPermission = "offline" | "media" | "speech";

/** An app's manifest: the OS enforces it, so the app can never exceed it (DV-3). */
export interface AppManifest {
  /** The app's identity: it acts "by <id> for <user>". */
  id: string;
  name: string;
  version: string;
  description?: string;
  /** The only actions the app may call. */
  actions: string[];
  permissions?: AppPermission[];
  /** The data labels it touches; the most sensitive bounds what it may open. */
  labels: Label[];
  /** None yet: apps reach the world only through actions. */
  network?: string[];
}

/** One declared action, and how it runs for this user today. */
export interface AppAction {
  name: string;
  /** "Send emails" */
  title: string;
  /** `ask`: the user approves each call on the desktop, not in the app. */
  tier: Tier;
  side_effect: SideEffect;
}

/** The app, opened for the signed-in user. */
export interface AppSession {
  app: string;
  name: string;
  version: string;
  /** Who the app acts for. */
  user: string;
  actions: AppAction[];
  permissions: AppPermission[];
}

/** What a call did. Refusals (undeclared, blocked, declined) are `ok: false`. */
export interface AppCallResult<T = unknown> {
  ok: boolean;
  text: string;
  data: T;
}

/** What the user is being asked on the desktop. */
export interface AppWaiting {
  summary: string;
  items: string[];
}

export interface AppStatus {
  live: boolean;
  waiting: AppWaiting | null;
  /** Pass as `since` to hear of the next change. */
  cursor: number;
  /** Apps whose data changed for the person since `since`. */
  changed: string[];
  /** Each declared action's tier now: IT's new policy changes it while the app is open. */
  actions?: AppAction[];
}

const ID = /^[a-z][a-z0-9-]{0,62}$/;
const ACTION = /^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+$/;
const LABELS: readonly string[] = ["public", "internal", "confidential", "restricted"];
const PERMISSIONS: readonly string[] = ["offline", "media", "speech"];
const FIELDS: readonly string[] = [
  "id",
  "name",
  "version",
  "description",
  "actions",
  "permissions",
  "labels",
  "network",
];

/**
 * What's wrong with a manifest, the way glassd will see it
 * (`AppManifest::check`), so `make apps-lint` catches it before install.
 */
export function manifestProblems(value: unknown): string[] {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return ["a manifest is a JSON object"];
  }
  const m = value as Record<string, unknown>;
  const problems: string[] = [];
  for (const key of Object.keys(m)) {
    if (!FIELDS.includes(key)) problems.push(`unknown field ${key}`);
  }
  const line = (field: string, max: number) => {
    const text = m[field];
    // Control characters (a newline forging a second line) are refused.
    const control = /[\u0000-\u001f\u007f-\u009f]/;
    if (
      typeof text !== "string" ||
      !text.trim() ||
      [...text].length > max ||
      control.test(text)
    ) {
      problems.push(`${field} must be one line of 1 to ${max} characters`);
    }
  };
  if (typeof m.id !== "string" || !ID.test(m.id)) problems.push("id must be a slug like mail");
  line("name", 40);
  line("version", 32);
  const actions = Array.isArray(m.actions) ? m.actions : [];
  if (actions.length === 0) problems.push("declares no actions");
  const seen = new Set<string>();
  for (const action of actions) {
    if (typeof action !== "string" || !ACTION.test(action)) {
      problems.push(`${String(action)} is not an action name (no patterns)`);
    } else if (seen.has(action)) {
      problems.push(`declares ${action} twice`);
    }
    seen.add(String(action));
  }
  const labels = Array.isArray(m.labels) ? m.labels : [];
  if (labels.length === 0) problems.push("declare the data labels the app touches");
  for (const label of labels) {
    if (!LABELS.includes(String(label))) problems.push(`unknown label ${String(label)}`);
  }
  if (labels.includes("restricted")) problems.push("no app may open restricted data");
  for (const permission of Array.isArray(m.permissions) ? m.permissions : []) {
    if (!PERMISSIONS.includes(String(permission))) {
      problems.push(`unknown permission ${String(permission)}`);
    }
  }
  if (Array.isArray(m.network) && m.network.length > 0) {
    problems.push("network destinations aren't supported yet; apps reach the world through actions");
  }
  // CF-21: the camera and microphone are for meetings; Chat never has them.
  const permissions = Array.isArray(m.permissions) ? m.permissions.map(String) : [];
  const declared = Array.isArray(m.actions) ? m.actions.map(String) : [];
  if (permissions.includes("media") && !declared.includes("meet.join")) {
    problems.push("media is for meetings: only an app that declares meet.join may have it");
  }
  if (permissions.includes("speech") && !declared.includes("meet.transcribe")) {
    problems.push("speech is for transcribed meetings: only an app that declares meet.transcribe may have it");
  }
  return problems;
}
