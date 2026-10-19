// "by [agent] for [user]" (AU-10): who wrote something the OS wrote for
// someone. First-party apps show it inline wherever an agent wrote or
// changed what's on screen.

export interface Attribution {
  /** The whole line, as the OS recorded it. */
  text: string;
  /** The agent (or app) that wrote it. */
  by: string;
  /** The user it acted for. */
  for: string;
}

/** Reads "by invoice-demo for demo". Anything else isn't an attribution. */
export function parseAttribution(text: string | null | undefined): Attribution | null {
  const line = (text ?? "").trim();
  if (!line.startsWith("by ")) return null;
  const rest = line.slice(3);
  const split = rest.lastIndexOf(" for ");
  if (split <= 0) return null;
  const by = rest.slice(0, split).trim();
  const user = rest.slice(split + 5).trim();
  if (!by || !user) return null;
  return { text: line, by, for: user };
}

/**
 * Whether to show the line. What the user wrote through this app ("by mail
 * for demo") is just their own mail; what an agent wrote always shows.
 */
export function showAttribution(attribution: Attribution | null, app: string): boolean {
  return attribution !== null && attribution.by !== app;
}
