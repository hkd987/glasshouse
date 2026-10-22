# 0041. Tool use through the model gateway, and a first desktop agent

Status: **Proposed, built** (2026-10-21). Built overnight on the
owner's standing ask to get our agents hooked up and tested
(2026-10-21), without their sign-off on two choices, named below, for
them to confirm. Built: everything in the decision below, on every
provider, tested with the scripted provider (unit, runtime and the VM
suite, where the desktop agent runs in its jail); no run on a real model
yet. PRD AE-3, MP-7, DP-1, DP-3, DP-5. Extends ADR 0018 (the
model gateway); ADR 0010's harness bundles build on it later.

## Context
- `model_complete` (ADR 0018) is text in, text out. An agent that wants a
  model to choose its next step has to ask for JSON in words and parse
  it, as the IT agent does today (ADR 0039 follow-up). That's brittle, and
  every model provider offers a structured way: Anthropic's `tools` and
  `tool_use` blocks, OpenAI-style `tools` and `tool_calls` (OpenRouter),
  Bedrock Converse's `toolConfig`.
- AE-3 asks for a general desktop agent: the default for anything typed
  in the command bar, which plans and picks the right step. Today no
  agent of ours can do that.
- The containment line (ADR 0004, PA-3): an agent reaches anything only
  through the actions its task socket offers, each decided by policy
  (tier, ask, Never, labels, scope) and audited.

## Decision
1. **The model proposes; the task socket decides.** A model's tool call
   is a suggestion handed back to the agent as data. The agent makes the
   call itself, over its task socket, where every rule applies exactly as
   if the agent had chosen it: tiers and asks, Never, labels, scope,
   undo, audit. The gateway never runs a tool. *(Choice 1 for the owner:
   this is the safety property; it rules out letting a provider call
   tools server side.)*
2. **`model_complete` takes tools and returns tool calls.** The input
   gains `tools` (name, description, JSON Schema input; at most 64), and
   messages may carry `tool_calls` (an assistant's) and `tool_results` (a
   user's, each answering a call in the message before by its id). The
   reply's text is the model's words; its data carries `tool_calls` and
   `stop_reason: "tool_use"` when the model wants tools run. *(Choice 2:
   this wire shape, neutral across providers, closest to Anthropic's.)*
3. **Redaction covers tools too (DP-3).** Every string in a tool result
   and in a tool call's input is redacted on the way out, as message text
   is: a tool result is file contents or mail. Placeholders the model
   writes into a tool call's input come back with the personal details
   they stand for, as in text replies; secrets don't. Tool definitions
   are the OS's own (`tools/list`) and go as they are.
4. **The record keeps counts, never contents (DP-5).** A call's audit
   record says how many tools were offered and which ones the model
   called, never their inputs or results, unless policy logs prompts.
5. **Each provider maps it.** Anthropic Messages, OpenRouter (OpenAI
   style), Bedrock Converse, model credits through the console (the
   computer's call carries tools to the console, which forwards them),
   and the scripted provider, which calls a tool deterministically so the
   suites need no model.
6. **`glass-desktop-agent`, a first general agent (AE-3).** It offers
   the model the tools its task socket lists, runs the calls the model
   asks for (over the socket, so asks show and refusals come back as
   results), feeds the results back, and stops when the model answers in
   words, after at most 20 steps, or when the model can't be reached. Its
   default rules are narrow: reading files, mail, calendar and people,
   and drafting; everything else at its tier. Enter in the command bar
   with no app found hands it what was typed, as a task with no folders,
   which opens on screen. Its answer is kept on the task
   (`TaskDetail.answer`, at most 4,000 characters, control characters
   stripped like any agent text); `glassctl show` and the shell's task
   view show it.

## Consequences
- Every agent can let a model choose actions without parsing prose, and
  none can do more than its task socket allows.
- Tool results leave the machine, redacted, on the route the task's
  labels allow: an agent that reads a confidential file and asks a model
  sends it only where confidential data may go.
- Tool definitions add tokens to every call that offers them.
- The desktop agent sends the model a tool's data as well as its words
  (2026-10-22): `mail.list` says "7 messages in Inbox" and puts the
  messages in its data, so a model given only the words, as it was until
  then, could see no sender, subject or event on a real model. glassd's
  task socket keeps the two apart (the words are what tiles show);
  whether Claude Code, on the same socket, shows its model the data is
  undocumented.
- The desktop agent's first message says when it is on the computer
  (`GLASS_UTC_OFFSET`, which glassd gives every agent): a model has no
  date of its own, and calendar times are local. It goes in the message,
  not the instructions, which stay the same so the prompt cache keeps
  them (ADR 0043).

## Not built yet
- Streaming, parallel tool calls run concurrently (they run in order),
  images, and a model's server-side tools.
- The desktop agent handing off to specialists (AE-3's second half), its
  answer in the command bar itself, and folders chosen from the bar.
