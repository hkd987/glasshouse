// The Glasshouse Agents API (crates/lib/agents-api, ADR 0065 §12, ADR 0066):
// JSON over the front's own origin (`glass-agents`), signed in by the
// org's SSO in front of it. These types mirror the Rust ones field for
// field; a refusal is `{"error": "..."}`, shown to the person as it is.

export const PREFIX = "/api/v1";

export interface Session {
  person: Person;
  org: string;
}

export interface Person {
  /** The account name: `dana`. */
  id: string;
  name: string;
  email?: string;
}

/** `POST /api/v1/session`, development only. */
export interface SignIn {
  person: string;
}

export type AgentStatus = "starting" | "working" | "needs_you" | "taken_over" | "idle" | "asleep";

export type ComputerState = "new" | "starting" | "ready" | "running" | "saving" | "asleep" | "waking";

/** An agent, as its card shows it. */
export interface AgentView {
  /** `reconciler`: letters, digits and `-`. */
  id: string;
  name: string;
  /** What it's for: "Finance". */
  role: string;
  /** The template it was made from. */
  template: string;
  status: AgentStatus;
  /** What it's doing, or last did, in a line. */
  doing: string;
  /** When that last changed (RFC 3339). */
  updated_at: string;
  /** Questions waiting on the person. */
  pending: number;
  computer: ComputerView;
}

export interface ComputerView {
  state: ComputerState;
  /** What runs it, for people: "Firecracker microVM", "Linux namespaces". */
  kind: string;
  /** Whether it is a security boundary. */
  contained: boolean;
  disk_bytes?: number;
  /** Asleep: what it takes in storage, and since when. */
  stored?: Stored;
  /** It has a display of its own, a Glasshouse OS desktop, to watch and,
   * taken over, to drive (ADR 0072); otherwise its screen is a terminal. */
  display?: boolean;
}

/** `GET /api/v1/agents/<id>/display?since=<version>`. */
export interface Display {
  version: string;
  width: number;
  height: number;
  /** PNG in base64; left out when it's the version asked about. */
  png?: string;
  taken_over: boolean;
}

/** One thing the person does on the display, while they have it. */
export type DisplayInput =
  | { kind: "move"; x: number; y: number }
  | { kind: "button"; button: "left" | "middle" | "right"; down: boolean }
  | { kind: "wheel"; dy: number }
  | { kind: "key"; code: string; down: boolean };

export interface Stored {
  bytes: number;
  at: string;
  /** Where, for people: "S3: acme-glasshouse-agents". */
  place: string;
}

/** `GET /api/v1/agents/<id>`. */
export interface AgentDetail {
  agent: AgentView;
  reach: Reach[];
  messages: Message[];
  /** The latest of what it did, on the audit record, newest last. */
  record: RecordLine[];
}

export interface Reach {
  text: string;
  sub: string;
  ok: boolean;
}

export interface RecordLine {
  at: string;
  text: string;
}

export type From = "person" | "agent";

export interface Message {
  /** Unique in the conversation, in order. */
  id: string;
  at: string;
  from: From;
  /** The task it belongs to. */
  task: string;
  body: Body;
}

export type Body =
  | { kind: "text"; text: string; files?: string[] }
  | { kind: "progress"; steps: string[] }
  | ({ kind: "approval" } & ApprovalView)
  | { kind: "done"; text: string; changed?: string[]; undoable: boolean }
  | { kind: "note"; text: string };

export type Channel = "web" | "desktop" | "cli" | "teams" | "api";

export type ApprovalState =
  | { state: "pending" }
  | { state: "approved"; by: string; at: string; via: Channel }
  | { state: "denied"; by: string; at: string; via: Channel }
  | { state: "expired" };

/** A question an agent asked. */
export interface ApprovalView {
  id: string;
  /** The action it asks for: `mail.send`. */
  action: string;
  /** What it wants to do, for people. */
  summary: string;
  detail?: string;
  /** The answer's button: "Send 3 emails". */
  yes_label: string;
  state: ApprovalState;
  /** Its detail stays out of Teams and notifications (ADR 0018). */
  confidential?: boolean;
}

/** `GET /api/v1/approvals`. */
export interface PendingApproval {
  agent: string;
  agent_name: string;
  approval: ApprovalView;
}

/** `POST /api/v1/approvals/<id>`. */
export interface Answer {
  yes: boolean;
  via?: Channel;
}

/** `POST /api/v1/agents/<id>/ask`. */
export interface Ask {
  text: string;
  files?: string[];
  via?: Channel;
}

export interface Asked {
  task: string;
}

/** `GET /api/v1/agents/<id>/screen?since=<offset>`. */
export interface Screen {
  /** Where the next read starts. */
  offset: number;
  text: string;
  /** Whether the person has it now. */
  taken_over: boolean;
  state: ComputerState;
}

/** `POST /api/v1/agents/<id>/input`, taken over only. */
export interface Input {
  line: string;
}

export interface UndoResult {
  text: string;
  /** Files it couldn't put back, and why. */
  left?: string[];
}

/** An agent IT approved, to make one from. */
export interface Template {
  id: string;
  name: string;
  team: string;
  does: string;
  reach: Reach[];
}

/** `POST /api/v1/agents`. */
export interface NewAgent {
  template: string;
  name?: string;
}

/** `GET /api/v1/events?since=<cursor>`, waiting up to 25 s. */
export interface Events {
  cursor: number;
  /** Agents whose card, conversation or computer changed. */
  agents: string[];
}

/** Whether `id` names an agent: 1 to 40 of `a-z`, `0-9` and `-`, not
 * starting with `-` (agents-api's `valid_id`). */
export function validId(id: string): boolean {
  return /^[a-z0-9][a-z0-9-]{0,39}$/.test(id);
}

/** Signed out: the org's sign-in comes next. */
export class SignedOut extends Error {}

/** The front said no, in words for people. */
export class Refused extends Error {
  readonly status: number;
  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

/** The front can't be reached. */
export class Unreachable extends Error {}

/** One call to the front. POSTs always carry a JSON body (`{}` when the
 * route takes none), so a cross-site form can't make one. */
export async function api<T>(method: "GET" | "POST", path: string, body?: unknown, signal?: AbortSignal): Promise<T> {
  let response: Response;
  try {
    response = await fetch(PREFIX + path, {
      method,
      headers: method === "POST" ? { "content-type": "application/json", accept: "application/json" } : { accept: "application/json" },
      body: method === "POST" ? JSON.stringify(body ?? {}) : null,
      credentials: "same-origin",
      cache: "no-store",
      signal: signal ?? null,
    });
  } catch (error) {
    if ((error as Error).name === "AbortError") throw error;
    throw new Unreachable("Glasshouse Agents can't be reached right now.");
  }
  const data = (await response.json().catch(() => null)) as unknown;
  const words = data && typeof data === "object" && typeof (data as { error?: unknown }).error === "string" ? (data as { error: string }).error : "";
  if (response.status === 401) throw new SignedOut(words || "Sign in again to see your agents.");
  if (!response.ok) throw new Refused(response.status, words || `Something went wrong (${response.status}). Try again in a moment.`);
  return data as T;
}

/** `/agents/<id>` and below, with the id checked and encoded. */
export function agentPath(id: string, rest = ""): string {
  if (!validId(id)) throw new Refused(404, "There's no agent by that name.");
  return `/agents/${encodeURIComponent(id)}${rest}`;
}
