# 0042. The desktop agent on the person's own Claude Code and Claude plan

Status: **Proposed, first slice built** (2026-10-21). The owner asked for
it: "the ability to work with the user's Claude Code harness so we can use
that subscription if they have one." PRD MP-5, HX-1, HX-2, HX-3, HX-7,
AE-3. Builds on ADR 0015 (attended harnesses) and ADR 0041 (the desktop
agent).

## Context
- MP-5: bring your own subscription "the way each provider allows it:
  Claude plans through Claude Code itself". HX-7: an external harness
  sends model traffic straight to its provider; our MCP server enforces
  data labels, and confidential data is blocked for personal subscriptions
  by default. HX-2: each Claude Code session runs in an agent jail.
- Today Claude Code is an attended catalog agent the person connects by
  hand (`glassctl run claude-code`, ADR 0015). Nothing starts it for them,
  and the desktop agent (ADR 0041) runs only on models through our gateway.
- **Anthropic's terms** (docs read 2026-10-21):
  - Allowed: "Nor does it prevent an end user from signing in to the
    unmodified Claude Code binary with their own Claude subscription,
    including where a platform hosts Claude Code."
    (code.claude.com/docs/en/legal-and-compliance)
  - Not allowed: "developers may not collect, store, or intermediate
    Claude.ai credentials or session tokens — sign-in to a Claude account
    must complete through Anthropic's own flow", and "Anthropic does not
    permit third-party developers to offer Claude.ai login into their own
    applications, or to route requests through Free, Pro, or Max plan
    credentials on behalf of their users." (same page)
  - Preinstalling or running Claude Code in a product "requires agreeing to
    our Commercial Terms of Service". The binary must not be modified, and
    no sign-in method it has may be removed or restricted. Claude Code and
    Anthropic names may not be part of our product's name or logo; saying
    in plain text that it runs Claude Code is fine.
  - The Agent SDK: "Unless previously approved, Anthropic does not allow
    third party developers to offer claude.ai login or rate limits for
    their products". (code.claude.com/docs/en/agent-sdk/overview)
  - Unsettled: whether a product that launches the person's own installed
    Claude Code counts as "running Claude Code in your products".
- **Platforms:** Claude Code runs on macOS, Windows and Linux
  (code.claude.com/docs/en/setup). There is no FreeBSD build, and its npm
  package installs a per-platform native binary, so it doesn't run on the
  Glasshouse OS today.
- Headless mode (`claude -p`) can turn off every built-in tool
  (`--tools ""`), load only one MCP server (`--strict-mcp-config
  --mcp-config`), run without prompts (`--permission-mode dontAsk` with our
  tools allowed) and stream events (`--output-format stream-json`).
  `--bare` would isolate it further but never reads a plan's sign-in.

## Decision
1. **Glasshouse launches the person's own Claude Code; it never touches
   their sign-in.** The person installs the unmodified Claude Code and
   signs in through Anthropic's own flow. Glasshouse starts it, as them, in
   their session, with their own environment. It never reads, stores,
   refreshes or passes on Claude credentials, and never calls Anthropic's
   API with them.
2. **A driver, run by the person's client, never by glassd.** The new
   `desktop-agent-plan` catalog entry ("Desktop agent (your plan)") is
   attended (ADR 0015): glassd only makes its task socket. Its `driver`,
   `glass-claude-runner`, is run as the person by whoever started the task
   (`glassctl run --drive` today, the command bar next) with the socket in
   `GLASS_AGENT_SOCKET`. When it ends, the client hands the task back
   (Done, with the answer) or stops it (with the reason on the tile).
3. **This computer's tools only, proven before its first step.** The
   runner starts `claude -p` in an empty folder of its own, the request on
   stdin, our desktop agent's instructions appended to Claude Code's own,
   `--tools ""`, `--strict-mcp-config` with one server (`glass-mcp` to the
   task socket), our tools allowed (`model_complete` denied: it is a model
   already), `--permission-mode dontAsk`, no CLAUDE.md, memory, slash
   commands, claude.ai connectors or saved session. Claude Code's first
   event lists the tools it has: anything but ours, or our server not
   connected, and the runner stops it before its first step, which waits
   on a model round trip. Every call it makes goes through the task
   socket: tiers, asks, Never, labels, scope, undo and the audit log apply
   as to any agent.
4. **Its traffic skips our gateway (HX-7), so its rules are narrower.**
   The entry has the desktop agent's actions and `max_label = "internal"`.
   No redaction applies to what Claude Code sends Anthropic, and no model
   call is recorded by us; each tool call is. It is in the default
   `everyone` group, so IT removes it in the console where personal plans
   aren't welcome.
5. **Where it runs, in steps.** First: Linux and macOS computers with
   Claude Code installed, which today means development hosts
   (`tools/dev/claude-plan.sh`). Next, for the Glasshouse OS: Claude Code
   on FreeBSD (from Anthropic), then in the agent's jail (HX-2) with
   egress to Anthropic only (HX-7).

## Consequences
- An agent of ours can run on a person's Claude plan with no key, credit
  or API account on our side, under the same rules as every agent.
- Its model traffic is outside our redaction and model record; labels and
  the task socket are the controls (HX-7).
- **Before shipping, the owner needs Anthropic's written confirmation**
  that launching the person's own Claude Code this way is permitted, and
  likely Commercial Terms.
- It can't run on the Glasshouse OS until Claude Code supports FreeBSD.
- Claude Code runs as the person, outside a jail, as attended Claude Code
  already does (ADR 0015). Its own tools are off, and the runner proves
  that before its first step. The jail (HX-2) remains the goal.

## Not built yet
- The command bar starting it, and a Settings row to choose it.
- Failed (rather than Stopped) when the driver can't finish.
- Running it in a jail, with egress to Anthropic only (HX-2, HX-7).
