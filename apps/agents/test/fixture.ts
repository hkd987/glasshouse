// Dana Alvarez's agents at Acme Corp, in the API's exact shapes
// (crates/lib/agents-api): what the dev server answers from and the unit
// tests read. Times are minutes before `now`, so the page reads as it
// would on the day.
//
// - Reconciler (Finance) matched March invoices and asks before sending
//   3 emails.
// - Researcher (Market research) finished and is asleep, its computer
//   saved to storage.
// - Contract reviewer (Legal) needs a confidential answer.

import type { AgentDetail, ApprovalView, Message, PendingApproval, Reach, Session, Template } from "../src/api.ts";

export interface World {
  session: Session;
  agents: AgentDetail[];
  templates: Template[];
  /** Each agent's terminal, as its computer printed it. */
  screens: Record<string, string>;
  /** What each agent says when asked something, in the dev server. */
  replies: Record<string, string>;
}

const PASSWORDS: Reach = { text: "Your passwords", sub: "never inside its computer: it asks the system to act for it", ok: false };

export function fixture(now: Date = new Date()): World {
  const at = (minutesAgo: number) => new Date(now.getTime() - minutesAgo * 60_000).toISOString().replace(/\.\d{3}Z$/, "Z");

  const reconcilerAsk: ApprovalView = {
    id: "ap-7f3c21",
    action: "mail.send",
    summary: "Send a reminder to the vendor that hasn’t been paid, and ask the two with an old bank account to confirm their details?",
    detail: "3 emails, from you, to Harbor Paper Co., Delta Freight and Mesa Office Supply",
    yes_label: "Send 3 emails",
    state: { state: "pending" },
    confidential: false,
  };

  const contractAsk: ApprovalView = {
    id: "ap-9b0e44",
    action: "mail.send",
    summary: "Send the marked-up Northwind services agreement to outside counsel?",
    detail: "1 email, from you, to Priya Shah at Baker & Lowe LLP, with northwind-msa-redline.docx attached",
    yes_label: "Send to counsel",
    state: { state: "pending" },
    confidential: true,
  };

  const reconciler: AgentDetail = {
    agent: {
      id: "reconciler",
      name: "Reconciler",
      role: "Finance",
      template: "reconciler",
      status: "needs_you",
      doing: "Matched March invoices; asks before sending 3 emails",
      updated_at: at(4),
      pending: 1,
      computer: { state: "running", kind: "Linux namespaces", contained: true, disk_bytes: 2_412_000_000 },
    },
    reach: [
      { text: "Finance/March", sub: "as copies: what it changes comes back for you to keep", ok: true },
      { text: "Your mail", sub: "drafts freely, asks before sending", ok: true },
      { text: "The web", sub: "only the sites IT allows", ok: true },
      PASSWORDS,
    ],
    messages: [
      text("m1", at(17), "person", "t-rec-1", "Match the March invoices against the bank export in Finance/March, and flag anything that doesn’t match."),
      text("m2", at(16), "agent", "t-rec-1", "On it. I copied two files into my computer to work on.", ["Finance/March/invoices-march.xlsx", "Finance/March/bank-export-march.csv"]),
      { id: "m3", at: at(9), from: "agent", task: "t-rec-1", body: { kind: "progress", steps: ["Read 214 invoices", "Matched 207 to bank payments"] } },
      { id: "m4", at: at(8), from: "agent", task: "t-rec-1", body: { kind: "progress", steps: ["Checked the 7 that don’t match"] } },
      {
        id: "m5",
        at: at(6),
        from: "agent",
        task: "t-rec-1",
        body: {
          kind: "done",
          text: "I made a sheet of the 7 that don’t match: 4 paid twice, 2 paid to an old bank account, 1 not paid yet.",
          changed: ["Finance/March/march-reconciliation.xlsx"],
          undoable: true,
        },
      },
      { id: "m6", at: at(4), from: "agent", task: "t-rec-1", body: { kind: "approval", ...reconcilerAsk } },
    ],
    record: [
      { at: at(17), text: "You asked it to match March invoices" },
      { at: at(16), text: "Copied 2 files from Finance/March into its computer" },
      { at: at(8), text: "Matched 207 of 214 invoices" },
      { at: at(6), text: "Wrote march-reconciliation.xlsx to Finance/March (undoable)" },
      { at: at(4), text: "Asked you before sending 3 emails (mail.send)" },
    ],
  };

  const researcher: AgentDetail = {
    agent: {
      id: "researcher",
      name: "Researcher",
      role: "Market research",
      template: "researcher",
      status: "asleep",
      doing: "Finished: a summary of logistics software pricing",
      updated_at: at(132),
      pending: 0,
      computer: {
        state: "asleep",
        kind: "Linux namespaces",
        contained: true,
        disk_bytes: 1_180_000_000,
        stored: { bytes: 412_000_000, at: at(132), place: "S3: acme-glasshouse-agents" },
      },
    },
    reach: [
      { text: "Research", sub: "as copies", ok: true },
      { text: "The web", sub: "sites IT allows; asks you for others", ok: true },
      { text: "Your mail", sub: "no access", ok: false },
      PASSWORDS,
    ],
    messages: [
      text("r1", at(220), "person", "t-res-1", "Pull together what’s public about mid-size logistics software vendors and summarize their pricing."),
      { id: "r2", at: at(175), from: "agent", task: "t-res-1", body: { kind: "progress", steps: ["Read 18 vendors’ public pages", "Kept notes in my computer"] } },
      {
        id: "r3",
        at: at(173),
        from: "agent",
        task: "t-res-1",
        body: {
          kind: "approval",
          id: "ap-12aa90",
          action: "web.fetch",
          summary: "The analyst report is on research.example.com, which your company’s rules don’t allow yet. Download it?",
          detail: "Allowing it once is for this task only. IT can add the site for everyone.",
          yes_label: "Allow once",
          state: { state: "approved", by: "dana", at: at(160), via: "teams" },
          confidential: false,
        },
      },
      {
        id: "r4",
        at: at(147),
        from: "agent",
        task: "t-res-1",
        body: {
          kind: "done",
          text: "Done: 18 vendors compared. Most charge per shipment above a base fee; three charge per seat. The summary has a table and my sources.",
          changed: ["Research/logistics-pricing.docx"],
          undoable: true,
        },
      },
      {
        id: "r5",
        at: at(132),
        from: "agent",
        task: "t-res-1",
        body: {
          kind: "note",
          text: "I went to sleep after 15 minutes with nothing to do. My computer is saved in your company’s storage and picks up where it left off when you ask me something.",
        },
      },
    ],
    record: [
      { at: at(220), text: "You asked it to compare logistics software pricing" },
      { at: at(175), text: "Read 18 vendors’ public pages" },
      { at: at(173), text: "Asked you about research.example.com (web.fetch)" },
      { at: at(160), text: "You allowed it once, in Teams" },
      { at: at(147), text: "Wrote logistics-pricing.docx to Research (undoable)" },
      { at: at(132), text: "Went to sleep; its computer saved to storage" },
    ],
  };

  const contracts: AgentDetail = {
    agent: {
      id: "contract-reviewer",
      name: "Contract reviewer",
      role: "Legal",
      template: "contracts",
      status: "needs_you",
      doing: "Marked up the Northwind agreement; needs your answer",
      updated_at: at(11),
      pending: 1,
      computer: { state: "running", kind: "Linux namespaces", contained: true, disk_bytes: 860_000_000 },
    },
    reach: [
      { text: "Contracts you give it", sub: "as copies", ok: true },
      { text: "Your mail", sub: "asks before sending", ok: true },
      { text: "The web", sub: "no access", ok: false },
      PASSWORDS,
    ],
    messages: [
      text("c1", at(48), "person", "t-con-1", "Compare the Northwind services agreement with our standard terms, and mark what differs.", ["Legal/Northwind/northwind-msa-v4.docx"]),
      { id: "c2", at: at(31), from: "agent", task: "t-con-1", body: { kind: "progress", steps: ["Read the agreement: 42 clauses", "Compared each with your standard terms", "Marked 9 that differ"] } },
      {
        id: "c3",
        at: at(12),
        from: "agent",
        task: "t-con-1",
        body: { kind: "done", text: "I marked 9 clauses that differ from your standard terms, with a note on each.", changed: ["Legal/Northwind/northwind-msa-redline.docx"], undoable: true },
      },
      { id: "c4", at: at(11), from: "agent", task: "t-con-1", body: { kind: "approval", ...contractAsk } },
    ],
    record: [
      { at: at(48), text: "You asked it to compare the Northwind agreement" },
      { at: at(47), text: "Copied northwind-msa-v4.docx into its computer" },
      { at: at(12), text: "Wrote northwind-msa-redline.docx to Legal/Northwind (undoable)" },
      { at: at(11), text: "Asked you a confidential question (mail.send)" },
    ],
  };

  return {
    session: { person: { id: "dana", name: "Dana Alvarez", email: "dana.alvarez@acme.example" }, org: "Acme Corp" },
    agents: [reconciler, contracts, researcher],
    templates: [
      {
        id: "contracts",
        name: "Contract reviewer",
        team: "Legal",
        does: "Reads the contracts you give it and marks what differs from your standard terms.",
        reach: [
          { text: "Contracts you give it", sub: "as copies", ok: true },
          { text: "Mail and the web", sub: "no access", ok: false },
        ],
      },
      {
        id: "notes",
        name: "Meeting notes",
        team: "Everyone",
        does: "Turns a meeting’s recording into notes and follow-ups, in your Drafts.",
        reach: [
          { text: "Recordings you share", sub: "as copies", ok: true },
          { text: "Your Drafts", sub: "asks before sending", ok: true },
        ],
      },
      {
        id: "expenses",
        name: "Expense checker",
        team: "Finance",
        does: "Checks your receipts against the expense policy before you submit them.",
        reach: [
          { text: "Receipts you give it", sub: "as copies", ok: true },
          { text: "The expense policy", sub: "read only", ok: true },
        ],
      },
      {
        id: "interviews",
        name: "Interview scheduler",
        team: "People",
        does: "Finds times that work for candidates and interviewers, and sends the invitations.",
        reach: [
          { text: "Calendars", sub: "free and busy only", ok: true },
          { text: "Invitations", sub: "asks before sending", ok: true },
        ],
      },
    ],
    screens: {
      reconciler: [
        "\x1b[32magent@reconciler\x1b[0m:\x1b[34m~\x1b[0m$ ls in/",
        "bank-export-march.csv  invoices-march.xlsx",
        "\x1b[32magent@reconciler\x1b[0m:\x1b[34m~\x1b[0m$ python3 match.py in/invoices-march.xlsx in/bank-export-march.csv",
        "Read 214 invoices, 231 bank payments",
        "Matching ████████████████████ 214/214",
        "Matched 207",
        "Not matched: 7",
        "  INV-2032  Delta Freight          3,615.50  old bank account",
        "  INV-2034  Orbit Cleaning           980.00  paid twice (Mar 6, Mar 9)",
        "  INV-2035  Mesa Office Supply     2,108.75  old bank account",
        "  INV-2037  Granite Courier        1,890.00  not paid",
        "  INV-2039  Orbit Cleaning           980.00  paid twice (Mar 12, Mar 14)",
        "  INV-2041  Harbor Paper Co.         655.20  paid twice (Mar 15, Mar 18)",
        "  INV-2044  Pine Street Print        290.00  paid twice (Mar 19, Mar 21)",
        "Wrote out/march-reconciliation.xlsx",
        "\x1b[32magent@reconciler\x1b[0m:\x1b[34m~\x1b[0m$ cp out/march-reconciliation.xlsx /changes/Finance/March/",
        "\x1b[32magent@reconciler\x1b[0m:\x1b[34m~\x1b[0m$ python3 draft_reminders.py out/march-reconciliation.xlsx",
        "Drafted 3 emails in Dana's Drafts",
        "Asking Dana before sending 3 emails (mail.send)… waiting",
        "",
      ].join("\r\n"),
      researcher: [
        "agent@researcher:~$ python3 summarize.py notes/ > out/logistics-pricing.md",
        "18 vendors, 41 price points",
        "agent@researcher:~$ pandoc out/logistics-pricing.md -o /changes/Research/logistics-pricing.docx",
        "agent@researcher:~$ ",
      ].join("\r\n"),
      "contract-reviewer": [
        "agent@contract-reviewer:~$ redline --standard terms/acme-standard-msa.docx in/northwind-msa-v4.docx",
        "42 clauses read, 9 differ",
        "Wrote out/northwind-msa-redline.docx",
        "Asking Dana before sending 1 email (mail.send, confidential)… waiting",
        "",
      ].join("\r\n"),
    },
    replies: {
      reconciler: "Seven of 214 don’t match: four paid twice, two paid to an old bank account, one not paid yet. They’re all in march-reconciliation.xlsx, one row each.",
      researcher: "From my notes: the cheapest plans start around $400 a month, and most vendors add a fee per shipment above a base amount. The summary’s second table has each one.",
      "contract-reviewer": "The biggest differences are unlimited liability in 12.3 and a 90-day payment term instead of 30. The redline has a note on each of the 9.",
    },
  };
}

function text(id: string, at: string, from: Message["from"], task: string, words: string, files?: string[]): Message {
  return { id, at, from, task, body: files ? { kind: "text", text: words, files } : { kind: "text", text: words } };
}

/** `GET /api/v1/approvals` from a world: each pending question, oldest first. */
export function pendingOf(world: World): PendingApproval[] {
  const out: PendingApproval[] = [];
  for (const detail of world.agents) {
    for (const m of detail.messages) {
      if (m.body.kind === "approval" && m.body.state.state === "pending") {
        const { kind: _, ...approval } = m.body;
        out.push({ agent: detail.agent.id, agent_name: detail.agent.name, approval });
      }
    }
  }
  return out;
}
