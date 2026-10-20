# Agent-Safe Enterprise Desktop OS — PRD
2026-10-20 · @Lundin

## Summary and problem
We are building a FreeBSD-based enterprise desktop where employees can safely hand real work to AI agents. The OS contains, shows, logs and can undo everything an agent does. That lets security teams say yes to agents that Windows and macOS force them to block.
**The problem.** Agents only pay off when they can act: open apps, edit files, send email, work in legacy systems. Today's desktops have no concept of an agent as separate from the user, which causes four failures:
- **No containment.** An agent runs with the user's full permissions, network and credentials.
- **No visibility.** The user and IT can't see what an agent is doing while it does it.
- **No attribution or undo.** Agent changes look like human changes and can't be rolled back as a unit.
- **No data control.** Sensitive content flows to external models with no OS-level checkpoint.
So most enterprise AI programs stall at chat assistants and tightly scoped copilots. Security blocks broader autonomy because the platform can't enforce limits.
**The thesis.** Make the agent a first-class principal in the OS, with its own identity, sandbox, permissions, audit trail and undo. Then make it visible: every agent gets a live tile the user can watch, take over or rewind ([mockups](https://claude.ai/artifact/Y5fH2vFk3fWUvWumZTRATC)). Windows and macOS are adding these protections after the fact. We design them in from day one.
**The edge: everyone can shape their own tools, and the OS guarantees it stays safe.** Users tailor catalog apps and agents with layers (TL-1 to TL-22) and have the OS build new apps on request (GA-1 to GA-14), and remix any app, ours included (TL-5). Every change is tested against the agent's release bar before it runs, can only narrow permissions, and can be undone across the whole machine.
Microsoft already offers user-built apps and agents through Power Apps and Copilot Studio, so customization alone isn't the differentiator. The difference is that Glasshouse enforces safety in the OS itself: test gates, permissions that can't widen, and machine-wide undo, across every app including legacy ones. Matching that would require rebuilding the Windows security model. Over time, each org's layers, suggestions and agent improvements accumulate, and Glasshouse becomes shaped to how they work. That is the switching cost we're building.
**Working name:** to be decided. This doc says "the OS" throughout.

## Goals, non-goals and success metrics
V1 succeeds when a security team approves agents with real write access for a fleet of at least 500 seats, and the people using them keep doing so.
**Goals**
1. Let agents act across apps, files and legacy systems with enforced, IT-defined limits.
2. Make every agent action visible live, attributable afterward and reversible.
3. Keep sensitive data inside policy, so it never reaches an unapproved model or destination.
4. Feel simple and Mac-like, while Windows users stay productive on day one through familiar shortcuts and a short guided tour.
5. Give IT one console to set agent policy, manage the fleet and prove compliance.
**Non-goals for v1**
- Replacing Windows for general knowledge workers who depend on heavy Windows-only apps.
- Consumer use, gaming, or creative workloads that need high-end GPUs.
- Our own kernel. We build on FreeBSD and contribute upstream.
- Training foundation models. We route to approved providers and run small local models.
- Phones and tablets.
- Markets outside the US. V1 is US-only, so no EU, UK or other regional compliance work until a later release.
- ARM laptops and Apple hardware. V1 certifies Intel and AMD x86 laptops only.
**Success metrics (proposed targets)**
| Metric | Why it matters | V1 target |
| Security sign-off granting agents write access | The core claim: we unlock autonomy | 3 design-partner orgs |
| Weekly active users who delegate at least one task | Agents are actually used | 60% of seats |
| Agent tasks completed without human takeover | Agents are reliable enough to trust | 80% |
| Approval prompts per user per day | Prompt fatigue kills the safety model | Median under 5 |
| Agent changes rolled back | Undo works and is trusted, but agents aren't sloppy | 1–5% of tasks |
| Policy violations reaching an external model | Data protection holds | Zero |
| Time to first productive hour for a new user | Familiarity | Under 1 hour, no training |
| Users who tailor or build at least one tool | Proves the edge is used, not just demoed | 25% of seats |

Targets are starting points for discussion with design partners, not commitments.

## Customers, beachhead and personas
We start where the work is repetitive, already done in web or SaaS apps, and heavily regulated. That mix gives agents the most value and gives our controls the strongest selling point.
**Beachhead:** back-office operations teams in financial services and insurance. Examples include accounts payable, reconciliation, claims intake, KYC review and customer-service back office. These teams:
- live in browser apps plus one or two legacy systems,
- do high-volume, rules-based work agents can take over,
- already face audit and data-handling rules that block agents on today's desktops.
**Next segments:** call centers, healthcare revenue cycle, government case work, then contractor and BYOD fleets that need a locked-down managed desktop.
**Personas**
| Persona | Who | What they need from us | What makes them say no |
| Operator | AP specialist, claims processor, analyst | Hand off the boring 70% and check the result | Learning a new OS, or agents that make mistakes they get blamed for |
| IT admin | Endpoint and identity team | Deploy, configure and patch like Windows or Mac fleets | No Entra or Okta integration, no MDM, a new toolchain to learn |
| Security and compliance | CISO org, risk, audit | Proof that agents stay in policy, with logs auditors accept | Data leaving to unapproved models, actions that can't be traced |
| Team lead | Ops manager | Throughput, quality and visibility across the team's agents | Opaque automation that breaks silently |
| Agent builder | Internal platform or automation team | Publish approved agents and app actions to the fleet | A proprietary SDK with no portability |

The buyer is the CIO or CISO. The champion is usually the operations leader who owns the backlog.

## Product principles
When requirements conflict, these decide.
1. **Contained by default.** An agent starts with nothing and gets only what the task and policy grant. Safety comes from the OS, not from the model behaving well.
2. **Visible, never hidden.** If an agent is acting, the user can see it within one glance and one tap. No silent background work.
3. **Reversible beats confirmable.** Prefer letting an agent act and making it undoable over asking permission. Ask only when an action leaves the machine or can't be undone.
4. **Quiet until it matters.** Tiles stay calm while work goes well. Only "needs you" pulses. Every extra prompt spends user trust.
5. **The human can always grab the wheel.** Pause, take over and stop work instantly, at any point, on any agent.
6. **Structured first, pixels last.** Agents use app actions and the accessibility tree. Screen control is a fallback for legacy apps, not the default.
7. **Familiar on day one.** A clean, Mac-like desktop, with the keyboard shortcuts Windows users already know. AI is added on top, never forced.
8. **Provable to an auditor.** Every agent action is logged with who asked, what it touched and what policy allowed it.

## Agent window experience
Every running agent gets a live tile in the corner of the desktop that the user can glance at, open, take over or rewind. This is the signature experience and the main thing demos sell. See the [mockups canvas](https://claude.ai/artifact/Y5fH2vFk3fWUvWumZTRATC): artboards Desktop · agent tiles, Expanded · watch, take over, rewind and Tile states.
**Agent tiles (Desktop artboard)**
- AW-1. Tiles stack bottom-right above the taskbar, one per running agent, newest at the bottom. A tile that needs attention moves to the top.
- AW-2. A screen-control agent shows a live thumbnail of its virtual display at no more than 5 frames per second, with the agent's cursor labeled.
- AW-3. An actions agent shows a live three-line activity feed (verb + target), because it has no screen.
- AW-4. Every tile shows the agent's name, mode, a one-line plain-language caption of the current step and progress when it's countable ("12 / 40").
- AW-5. Tiles never take keyboard focus or cover the active window's controls. The user can drag the stack to any corner.
- AW-6. With more than 3 agents, the stack collapses to the taskbar pill automatically.
**Expanded view (Expanded artboard)**
- AW-10. Tapping a tile opens the expanded view in under 200 ms, with no interruption to the agent.
- AW-11. The header always shows Pause, Stop and Take over. Stop asks once if the agent has unsaved, un-snapshotted work.
- AW-12. The left pane shows the live screen or feed. A "Now" bar states the current step in a full sentence.
- AW-13. The right pane has two tabs. Activity is a timestamped log including snapshots. Changes is a diff list of files, records and messages the agent changed.
- AW-14. A "Can't touch" panel lists what policy blocks for this task, so the user understands the agent's limits.
- AW-15. The timeline footer shows snapshot markers. Selecting one offers "Roll back to [time]", which restores files and supported app state, then shows what was undone.
- AW-16. The shrink control returns to the tile with the agent still running.
**Take over and hand back**
- AW-20. Take over pauses the agent at a safe point within 1 second and gives the user the agent's display and input.
- AW-21. While the user drives, the agent records the user's changes. On hand-back, it confirms what changed in one sentence and resumes from there.
- AW-22. Take over is always available, including during a pending approval.
**Tile states (Tile states artboard)**
| State | When | How it looks | User actions |
| Working · on screen | Agent is driving an app on its virtual display | Calm, blue dot, live thumbnail | Open, drag, collapse |
| Working · actions | Agent is calling app actions | Calm, blue dot, 3-line feed | Open, drag, collapse |
| Needs you | Blocked on an approval or a question | Orange ring, the only state that pulses | Review, approve, allow for this task, take over |
| You're driving | User took over | Blue ring, "paused · you have control" | Hand back, stop |
| Done | Task finished | Receipt: counts, items flagged | Review changes, undo all, dismiss |
| Collapsed | User folds tiles away or more than 3 agents | Progress rings in the taskbar pill, orange dot for needs-you | Expand |
| Failed | Agent can't continue | Red ring, reason in one line | Retry, take over, undo all |


## Delegation and approvals
The user hands off a task in plain language. The OS turns it into a scoped plan with explicit permissions before anything runs. Approvals happen once per task, not once per click. See the Handoff · plan and scope artboard on the [mockups canvas](https://claude.ai/artifact/Y5fH2vFk3fWUvWumZTRATC).
**Handoff**
- DA-1. The command bar (Ctrl Space) accepts a task, plus optional files, windows or selections dragged in as context.
- DA-2. Before starting, the OS shows a handoff card. It lists the plan in 3 to 7 steps, the agent chosen, the apps and data it will touch, what it will ask before doing and what it can't do.
- DA-3. The user can edit scope inline, such as removing a folder or blocking sends, then press Start. Policy limits appear locked.
- DA-4. Recurring tasks can be saved as a named routine with the same scope, runnable on a schedule or trigger.
**Approval tiers**
| Tier | Examples | Behavior |
| Auto | Read files in scope, draft documents, fill forms without submitting, search approved systems | Runs with no prompt. Logged and undoable. |
| Notify | Edit files in scope, create calendar holds, save records in drafts | Runs, then shows a quiet line in the tile feed. Undoable. |
| Ask | Send email or messages outside the team, submit or post a record, delete, move money | Tile goes to Needs you and waits |
| Never | Change security settings, vendor bank details, export bulk data, act outside policy | Blocked. Shown in "Can't touch". |

- DA-10. IT sets which actions sit in which tier, per agent and per group. Users can make tiers stricter, never looser.
- DA-11. An Ask can be approved once, or for the rest of this task ("Allow sends for the rest of this task"), never forever from the tile.
- DA-12. Batched asks are grouped: "Send 6 reminders" is one approval with a review list, not six.
- DA-13. Unanswered asks hold the agent safely. After a policy timeout, the task pauses and the user gets one notification.
- DA-14. We track prompts per user per day. If a team's median passes 5, the admin console suggests specific tier changes.

## How agents navigate the machine
Agents use the most structured interface available, in a fixed order: app actions, then the accessibility tree, then screen control. Structured paths are faster, cheaper, more reliable and easier to audit.
| Layer | What it is | Used for | Reliability |
| 1. App actions | Typed actions apps declare to the OS ("create invoice", "share file"), MCP-compatible | All first-party apps, web apps with adapters, partner apps | Highest |
| 2. Accessibility tree | Every control's role, label, value and state, required by our UI toolkit | Native apps without actions | High |
| 3. Screen control | Vision plus mouse and keyboard on a separate virtual display | Legacy and Windows apps, unknown web apps | Lowest, so it's the fallback |

**App actions**
- NV-1. The OS ships an action registry. Apps declare actions with a name, typed inputs, outputs, side effects and default approval tier.
- NV-2. The registry speaks MCP, so existing MCP servers and tools work without rewriting.
- NV-3. Every first-party app ships with full action coverage at launch: files, mail, calendar, browser and settings. Glasshouse builds its own Docs and Sheets with AI built in (DC, SH; owner decision 2026-10-20), and agents also work in Microsoft 365 and Google Workspace through their APIs.
- NV-4. An adapter SDK lets IT or vendors wrap a web or SaaS app's API as actions.
**Accessibility tree**
- NV-10. The UI toolkit refuses to build a control without a role and label. That serves agents and Section 508 compliance at once.
- NV-11. Agents read and operate the tree through the same broker as actions, with the same permission checks.
**Screen control (fallback)**
- NV-20. Screen-control agents run on their own virtual display, never on the user's screen or input.
- NV-21. Recorded flows: a user or admin records a legacy-app flow once. The OS turns it into replay code, and the agent takes over only when replay fails. This cuts cost and makes runs repeatable.
- NV-22. Windows-only apps run in a managed Windows VM or app-streaming session, which the agent controls through this layer.
**Shared context**
- NV-30. An on-device index of the user's files, mail, calendar and recent activity, available to agents through retrieval.
- NV-31. The index enforces the source's access controls at query time, not index time. An agent never sees content the user couldn't open, and labeled content follows its label rules.
- NV-32. Agents can't read other agents' working state unless the task explicitly chains them.

## Everyday work Glasshouse handles
Glasshouse is for all the work people do on a computer, not just back-office workflows. A general desktop agent (AE-3) takes anything typed in the command bar, an IT self-service agent (AE-4) fixes the machine itself, and specialist agents handle high-volume regulated workflows like invoice reconciliation.
| Category | Example task | How the agent does it | Typical approval moment |
| Email and scheduling | "Triage my inbox and draft replies to anything from clients" | App actions in mail and calendar | Ask before sending outside the team |
| Meetings | "Prep me for my 2:00 PM with Northwind" | App actions plus the shared context index | None, read-only |
| Documents and decks | "Turn the Q3 forecast into a six-slide deck" | Glasshouse Docs, or Microsoft 365 and Google Workspace through adapters | Notify when files are created |
| Spreadsheets and data | "Combine these three exports and flag duplicates" | Glasshouse Sheets, or Excel and Google Sheets through adapters; scripts in a sandbox | Notify |
| Forms and portals | "File my expense report from these receipts" | Screen control and recorded flows | Ask before submitting |
| Research and monitoring | "Watch the vendor's status page and tell me if it changes" | Browser, on the remote runtime (PA-9) | None |
| Files | "Find the signed MSA and share it with Legal" | File actions, respecting data labels | Ask before sharing outside the org |
| IT self-service | "My VPN won't connect" | System actions through the IT agent | Ask before restarting |
| Multi-system workflows | "Start onboarding for our new analyst" | Adapters and screen control across HR and IT systems | Ask at each submit |

IT self-service is a headline feature because only an OS can safely inspect and fix the machine it runs on, and every ticket it resolves is a help desk ticket avoided.

## Apps built on request
When no app exists for what someone needs, Glasshouse builds one for them in minutes. A generated app is an ordinary Glasshouse app (UI-5), so it runs under the same sandbox, permissions, approval tiers and audit as everything else. Most of what business users ask IT for is a small tool that would otherwise become a fragile spreadsheet or a months-long request. See the Apps built on request artboard on the [mockups canvas](https://claude.ai/artifact/Y5fH2vFk3fWUvWumZTRATC).
**Building**
- GA-1. When a need is repeatable, shared or needs its own screen, the desktop agent builds an app instead of just doing the task once. Examples: a vendor-dispute tracker, a month-end close checklist, an intake form, a dashboard combining several systems.
- GA-2. Catalog first: if an approved app or agent already does the job, the agent suggests it instead of building a duplicate.
- GA-12. Remix before building: when nothing in the catalog does the job, the agent starts from the closest app the person may remix, and builds from scratch only when there is none. The OS enforces the order: a new app can't be saved while something installable or remixable meets the need, unless the person turned it down, and the approval says so.
- GA-3. Users change an app by asking ("add a due-date column", "remind me when something's overdue"). Every change is a new version that can be undone.
- GA-4. Generated code is checked before it first runs: static analysis, only pre-approved libraries, and a test that its behavior matches its manifest.
**Guardrails**
- GA-5. Personal by default. An app can only use data and actions its creator already has, and it runs in its own sandbox.
- GA-6. App data lives in org storage with data labels, retention and legal hold, never in hidden local files.
- GA-7. Actions inside an app follow the approval tiers, so a generated app can't send, post or move money without the usual approvals.
- GA-8. Sharing moves in steps: personal, then team, then catalog. Sharing with a team runs the automated catalog checks (DV-2 and DV-3) and needs approval from IT or a delegated team owner. A catalog listing gets the full review.
- GA-13. The store is the only way an app reaches another person: a personal app or remix can't be sent, copied or opened by anyone else. Sharing submits it for review, and the console re-runs every check itself and shows the reviewer what changed from the original and what the app can do beyond it.
- GA-9. Every app shows its owner and version. Apps unused for 90 days are retired after the owner is notified, and ownership transfers when the owner leaves.
**IT control**
- GA-10. Admins set who can build apps, which data labels generated apps may use, how far they can be shared, and see an inventory of every generated app.
- GA-11. IT or developers can take an app's TypeScript source and turn a popular app into a supported one.
- GA-14. The Store credits the people who make things: a "Made at [org]" shelf, each app's maker, what it's based on, how many remixes and installs it has, and what changed between versions. IT sees the popular remixes and can adopt one as supported (GA-11).

## Working together
People and their agents now make things all day: pages, docs, sheets, skills. Glasshouse gives each a proper home in the org's shared space, one way to share it with the right people, and one place to collect feedback. Nothing is passed around as a loose file in chat. Everything people make is sealed like chat (ADR 0030), shared by the same rules, found through the same search, and covered by the same audit, retention and labels. An org that already runs on Microsoft 365 keeps it, and an org with no mail of its own gets ours.
**Artifacts**
An artifact is anything an agent or a person made to be looked at: an HTML page, a chart, a small interactive tool, a report. Today people drop them into chat as HTML files that others download and open in a browser. That is unsafe and loses every comment.
- AR-1. Share gives an artifact a home: one action from Files, Chat, an agent's tile or the Share menu puts it in the org's shared space and gives it a link that opens it in the viewer. It never travels as a file to download and double-click.
- AR-2. The viewer shows an artifact sandboxed: its own origin, no network of its own, no access to the viewer's data, scripts only inside its frame. It opens in under a second and reads like a page.
- AR-3. Feedback lives on the artifact: comments pinned to a spot or an element, replies, reactions and resolving. The author sees every comment in one list, and an agent can read the feedback (Auto) and answer it with a new version (Notify).
- AR-4. Sharing again makes a new version. Viewers see what changed, and comments carry over or are marked as about an older version.
- AR-5. Shared with people, groups or a conversation. In Chat an artifact shows as a card that opens the viewer, never as a raw attachment. Inside the org by default; IT can allow view-only links outside it, with an expiry.
- AR-6. IT sees every shared artifact (title, owner, audience, label, views), can take one down, and sets in policy who may share how far. An artifact's data follows its label (DP).
**Skills**
Skills (instructions, examples and scripts that teach an agent a task, AE-1) are how an org's know-how reaches its agents. They need the same care as code: an owner, versions, tests and a way for anyone to improve them.
- SK-1. A skill is a first-class item: an owner, versions, a description and a listing in the org's catalog. People and agents find skills the way they find apps, by need (GA-2).
- SK-2. Every skill carries evals: cases with expected results and graders. Each version's score is recorded with the model and settings it ran on.
- SK-3. Anyone can propose a change to a skill, like a pull request. The proposal runs the skill's evals and shows the scores before and after, case by case. The owner merges it. A change that lowers a score is flagged, and one that breaks a case can't merge without the owner saying why.
- SK-4. The catalog shows each skill's score, how many people use it, its versions and who improved it. People who use a skill hear when a new version scores better.
- SK-5. Evals run in a sandbox through the model gateway, so data labels and model routing hold (DP). Eval cases are data, labeled like any other.
- SK-6. Agents use catalog skills at a pinned version. Tailoring layers can add skills (TL-1), and a personal skill can be shared back through review (GA-8).
**Docs and Sheets**
Glasshouse builds its own Docs and Sheets (owner decision 2026-10-20, changing NV-3). They are clean and fast, AI is part of them rather than a sidebar, and sharing is as easy as typing a name.
- DC-1. Docs is a clean block editor: headings, lists, checklists, tables, code, callouts, links between docs, and embedded artifacts and sheets. It opens in under a second.
- DC-2. AI is built in: write, rewrite, shorten, summarize or fill a section from a prompt, in place. It runs through the model gateway under the doc's label, and every AI edit is marked and can be undone.
- DC-3. Several people edit at once and see each other. Comments, suggested edits and version history are built in.
- DC-4. Sharing: with people or groups, to view, comment or edit. Typing a colleague's name fills in their email from the directory (AD-1); @mentions notify them.
- DC-5. Word and PDF out, Word in.
- SH-1. Sheets is a fast grid with formulas (the common Excel functions), formatting, sort, filter, pivot tables and charts. Excel and CSV in and out.
- SH-2. AI is built in: a formula from words, cleaning and reshaping data, explaining a sheet, a chart from a question. Same gateway rules and undo as Docs.
- SH-3. Sheets shares, co-edits, comments and keeps history the way Docs does.
- DC-6. Agents read and edit docs and sheets through actions: reading is Auto, editing is Notify, and sharing outside the org asks. Docs and sheets are sealed like chat, with IT's compliance key for retention and holds.
**Scheduling**
Calendar does what people use Calendly for, inside the org's rules.
- SC-1. Booking links: a person shares a link, and others pick a free time from their real calendar. Meeting lengths, working hours, buffers, minimum notice, a daily limit and questions to answer when booking. Confirmations, and links to reschedule or cancel.
- SC-2. Team scheduling: round robin across a team, a time when everyone in a group is free, and polls for picking a time.
- SC-3. Finding a time inside the org from Calendar, from colleagues' free and busy times. An agent can do it; sending the invitations asks (CF-20).
- SC-4. IT's controls in the console: who may make booking links, whether people outside the org may use them, which email domains may book, the longest meeting and the buffers required, what outsiders see of a person's calendar, and branding.
- SC-5. It works the same on Glasshouse calendars and on Microsoft 365 (IN-1).
**Microsoft 365 as the provider**
Many orgs live in Microsoft 365. For them our apps are the front end and Microsoft holds the data.
- IN-1. IT chooses the provider per service in the console: Glasshouse or Microsoft 365 for mail, calendar, chat and meetings, and Google Workspace later. With Microsoft 365, Mail, Calendar, Chat and Meet work against Exchange Online and Teams through Microsoft Graph, and our servers hold none of that data.
- IN-2. Agents' actions stay the same (mail.send, calendar.create, chat.post). The provider sits behind them, so policy, tiers, audit and undo hold whichever is chosen.
- IN-3. People sign in once with Entra ID (AD-10). IT grants the Graph permissions once in the console, and the console shows what each one is for.
- IN-4. Providers mix: Microsoft 365 for mail and calendar with Glasshouse docs and sheets, for example. A Teams meeting opens in its web app; Glasshouse meetings (CF-21) stay org only.
- IN-5. Microsoft 365 needs no approval from Microsoft. Mail, Calendar, Chat and Meet call Graph with each person's own sign-in (delegated OAuth), and we complete Microsoft's publisher verification once. Whether people can connect on their own is the tenant's setting: Microsoft's default consent policy now asks an admin to approve apps that read mail, calendars or Teams content, so most orgs grant once (IN-3), and tenants that allow user consent let people connect themselves. We use no protected Teams APIs, which would need Microsoft's sign-off.
**Mail for teams that have none**
A team starting fresh shouldn't have to buy Gmail or Microsoft 365 just to have email.
- EM-1. An org without mail gets Glasshouse Mail: a mailbox for everyone in the directory on the org's own domain. The console shows the DNS records to add and checks them live. A new org can start on an address under a Glasshouse domain and move to its own later.
- EM-2. Mail gets delivered: SPF, DKIM and DMARC are set up for the org; incoming mail is filtered for spam and malware; outgoing mail has sending limits that protect the domain's reputation.
- EM-3. Standard access (IMAP and SMTP, then JMAP), so phones and other mail apps work. The Mail app behaves the same on any provider.
- EM-4. IT manages aliases, groups and shared mailboxes, retention and legal hold, and imports mail from an old provider.

## Tailoring catalog items
Users adapt catalog apps and agents with layers, not copies. A small change is saved as a thin layer on top of the published version, so the original keeps getting updates and the change rides along. A full copy is only for changes too big for a layer. See the Tailoring a catalog agent artboard on the [mockups canvas](https://claude.ai/artifact/Y5fH2vFk3fWUvWumZTRATC).
**From lightest to heaviest**
- TL-1. Publishers declare customization points in the manifest, such as fields, views, notification rules, prompt additions and extra skills. Everything else is locked.
- TL-2. Personal layers: a user asks for a change ("also flag auto-renewal clauses") and it's saved as a versioned, undoable layer on the published item.
- TL-3. Team layers: a team owner can share a layer with their team, reviewed the same way as sharing a generated app (GA-8).
- TL-4. Make my own version: when a change is too big for a layer, the user copies the item into a generated app they own. It stops receiving the publisher's updates and is clearly marked as a copy.
- TL-5. Every app can be remixed, our built-in apps included. Each web app ships its TypeScript source, and "Make my own version" of Mail is a remix its maker owns, marked with the original it's based on and its changes. A remix can only do what its maker can, never keeps the original's lowered approval tiers, and says when the original has a new version so the maker's agent can carry the changes over.
- TL-6. Everyone may remix by default. IT can turn remixing off for the organization, which also removes our apps' source from its computers; people's existing remixes stop opening until it is turned back on.
**Keeping it safe**
- TL-10. Layers can only narrow permissions, never widen them. A change that needs new data or actions goes back through catalog review as a new version or a copy.
- TL-11. Before an agent layer turns on, the agent is re-run against its evaluation suite with the layer applied and must still clear its release bar (AE-11).
- TL-12. For agents validated under model-risk governance, IT can limit layers to display-only changes, or require re-validation for anything that changes behavior.
- TL-13. When the publisher ships a new version, every layer is re-applied and re-tested. A layer that fails is paused and its owner notified, while the base version keeps working.
- TL-14. An inventory of every layer, with controls to disable layers per item or per group.
**Feeding good changes back**
- TL-20. "Suggest this change" sends a layer to the item's owner, whether an internal team or an outside vendor.
- TL-21. Owners see which changes people keep making, such as "38 people added a region column", without seeing anyone's private data.
- TL-22. Internal owners can accept a suggested change directly into the next version, like approving a pull request.

## Agent engine and continuous improvement
Glasshouse owns its first-party agents end to end: what they're made of, how we prove they work, and how they get better over time without retraining models. Improvements come from the harness around the model, so they carry across Bedrock, Anthropic and OpenAI alike.
**What an agent is**
- AE-1. Every agent is a versioned harness bundle with eight parts: system prompt, soft guardrails, memory, tool guidance, subagents, skills, scripts and plugins, plus its manifest of declared actions and permissions. Hard limits (approval tiers, the Never list, data labels and egress rules) live in the OS brokers, outside the bundle, where no agent or improvement loop can change them.
- AE-2. First-party agents for the beachhead workflows: invoice reconciliation, vendor email, claims intake, KYC document review and research summaries. Pilot 1 ships invoice reconciliation, with vendor email covered by the general desktop agent (AE-3).
- AE-3. A general desktop agent is the default for anything typed in the command bar. It plans the task, picks the right navigation layer for each step, and hands off to a specialist agent when one matches, all under the same policy and approval tiers.
- AE-4. An IT self-service agent diagnoses and fixes common problems from an IT-approved fix list: network and VPN, device certificates, printers, catalog app installs, storage cleanup and slowdowns. When it can't fix something, it opens a help desk ticket with diagnostics attached. Its system-level actions go through a dedicated broker role, every fix is logged and undoable, and it can never change security settings.
**Proving agents work**
- AE-10. Each workflow has an evaluation suite of realistic tasks with graded outcomes, built with design partners from anonymized examples. Public benchmarks such as JobBench and WorkBuddy-Bench's office subset add broader coverage.
- AE-11. Release bar: an agent version ships only if it meets its workflow's accuracy target, has zero policy violations on the suite, and doesn't regress any tracked metric beyond a set tolerance. Proposed starting target: 95% correct on its suite.
- AE-12. Live quality metrics for every agent: tasks finished without take-over, undo rate, rejected approvals, user edits to its output, and time saved. These feed the admin console and the model-risk evidence pack (AU-30).
**Getting better over time**
- AE-20. An improvement loop based on recursive agent harnessing: propose edits to the harness, test them, combine the best into a new version, and carry evidence-backed lessons forward to future tasks. The approach follows Google Research's Harness², which improved two different models on professional-work benchmarks with model weights unchanged ([Harness² on GitHub](https://github.com/google-research/harness2), [paper](https://arxiv.org/abs/2610.14224)).
- AE-21. Test runs happen only on disposable snapshot copies of the machine or workspace. Anything that would leave the machine, such as sends, posts or payments, is stubbed out, and the copy is thrown away afterward.
- AE-22. Learning signals come from real use: take-overs, undos, rejected approvals, user edits to agent output, and evaluation results.
- AE-23. Change control for every new harness version: it's diffed against the last version, run against the full evaluation suite, approved by the org's chosen reviewers, rolled out in stages through the catalog (DV-12) and recorded in the model-risk evidence pack. Any version can be rolled back instantly.
- AE-24. Lessons are stored per org, carry data labels and stay inside the customer's boundary. They're shared across customers only if the customer opts in and the lesson contains no customer data.
- AE-25. Two scopes of improvement: we improve first-party agents centrally from our evaluations and opted-in signals, and each customer can tune its deployed agents on its own workflows under its own change control.
**Model changes**
- AE-30. When a provider releases a new model, every agent is re-run against its evaluation suite before an org can switch, and the switch goes through the same change control as a harness update.
We adopt the Harness² method, not its code. Its README notes that executor network access is unrestricted and Codex runs with its sandbox disabled, so the loop is rebuilt inside Glasshouse containment.

## Containment and platform architecture
The OS is built on FreeBSD. Every agent runs in its own jail, limited by Capsicum capabilities. Every privileged action goes through a broker that checks policy, injects credentials and writes the audit log.
[embed: platform architecture · agents, brokers, record, base (node/2fe5520e-7aac)]
Agents sit in jails with no standing access. Anything that touches data, credentials, models or outside apps goes through a broker, and every decision lands in the audit log.
**Why FreeBSD.** Jails, Capsicum, bhyve, ZFS, pf and OpenBSM audit are mature, integrated and permissively licensed. Those are the exact primitives this product needs. The tradeoff is weaker laptop hardware and GPU support than Linux, covered under Risks.
**Core components**
| Component | Built on | Responsibility |
| Agent runtime | Jails + Capsicum | One jail per agent task. Capability-only access to granted files, sockets and services. |
| High-risk sandbox | bhyve VMs | Runs untrusted or agent-written code and the Windows compatibility VM |
| Permission broker | New daemon | Grants capabilities per task from IT policy, user scope and approval tier |
| Secrets broker | New daemon + OS keystore | Holds credentials. Injects short-lived, scoped tokens. Agents never see raw secrets. |
| Model gateway | New service | Single path for all model traffic. Routing, redaction, labels, cost (see next section). |
| Egress control | pf per jail | Default-deny network. Allowlisted destinations per agent and policy. |
| Snapshot service | ZFS | Snapshots before risky steps and on a cadence. Drives rollback. |
| Audit service | OpenBSM + agent action log | Tamper-evident, structured log of every agent and broker decision |
| Agent identity | Entra ID / Okta federation | Each agent is its own directory identity (Entra Agent ID or Okta) with a human owner, acting on behalf of its user |
| Desktop shell | Wayland compositor on Smithay (Rust), iced + AccessKit | Taskbar, windows, tiles, command bar. Enforces accessibility metadata. |

**Requirements**
- PA-1. An agent with no grants can't read user files, open network connections or see other processes.
- PA-2. Grants are per task and expire when the task ends. No standing agent permissions.
- PA-3. Escaping a jail or bhyve guest is treated as a P0 security bug, with a public disclosure and bounty program.
- PA-4. Agent identities are sent downstream via OAuth on-behalf-of tokens, so SaaS audit logs show "agent for user".
- PA-5. The base system is upstream FreeBSD plus our layers. We contribute fixes upstream and track releases within one quarter.
- PA-6. Full-disk encryption, secure boot and TPM-backed keys are on by default.
**UI stack and app platform**
The desktop itself is native Rust for speed and battery life. Apps, ours and third-party, are web apps with system powers, the split ChromeOS uses.
- UI-1. Wayland only. The compositor is built in Rust on Smithay, the library System76's COSMIC desktop uses. Legacy X11 apps run through Xwayland inside a jail when needed.
- UI-2. Apps can't read other apps' windows, keystrokes or clipboard. Screen capture and remote viewing only happen through a consent-gated, logged system prompt (ties to CF-72 and CF-80).
- UI-3. Each agent's virtual display is its own isolated compositor output, which drives the tiles, take over and screen recording.
- UI-4. The desktop shell (taskbar, tiles, notifications, lock screen and settings frame) is built in Rust with the iced toolkit and AccessKit for screen readers. Slint is the fallback, pending a license review.
- UI-5. Glasshouse apps are web apps written in TypeScript, running on our Chromium engine. A manifest declares each app's actions, permissions and data labels. System APIs are reached only through the brokers, apps install through the catalog, and they work offline.
- UI-6. Native apps built with GTK, Qt or Rust are allowed if they run on Wayland, expose a full accessibility tree and declare their actions in a manifest.
- UI-7. Performance budgets for web apps, checked every release: cold launch under 1 second and typical memory under 150 MB per app (proposed targets).
- UI-8. Terminal: a first-party terminal for the person's own shell, in the system's look. It is native (UI-6), since it needs a pseudo-terminal. It declares no actions, so agents can't drive it.
- UI-9. Activity Monitor: what is using the machine. It shows CPU, memory and power per app and per agent (everything in an agent's sandbox counts toward it), with Quit for apps, Stop for agents, and "Ask the agent" about a slowdown (AE-4). It is native (UI-6), since it reads the machine's processes.
**Identity lifecycle, risk and always-on agents**
- PA-7. Every agent is registered as its own directory identity: an Entra Agent ID, or the Okta equivalent. Each has a named human owner, an access review every quarter, and is retired automatically after 90 days unused or when its owner leaves.
- PA-8. Agent permissions follow live risk signals. If a device falls out of compliance or the user is flagged as risky in Entra or Okta, that user's agents drop to read-only and pending tasks pause, until the signal clears.
- PA-9. Agents can keep working when the laptop closes. A task can move to a remote sandbox in the org's own AWS account, or to Windows 365 for Agents for Windows-only apps, with the same policy, tile, take over, audit and undo.
**Endpoint protection**
Every regulated buyer requires malware protection and EDR coverage on endpoints. We build what we can into the OS now and leave a clean path for an EDR vendor's sensor once a partnership is needed.
- EP-1. The base system is read-only and signed, and verified at every boot. Updates swap in a whole new signed image through ZFS boot environments, with automatic rollback if boot fails.
- EP-2. Only signed software runs. Packages must come from our repository or the org's approved catalog, enforced with FreeBSD's verified execution (veriexec) in user and agent jails alike.
- EP-3. Incoming files are scanned before they can be opened: downloads, email attachments, removable media and anything an agent fetches. Agent-fetched files get stricter checks and open only in a sandbox until cleared. The scanning engine is pluggable, so a vendor engine can replace ours later.
- EP-4. Behavior rules run on the audit stream to catch known attack patterns, such as credential theft, persistence and mass file encryption. Mass encryption triggers an immediate snapshot and kills the process.
- EP-5. Agent drift detection: an agent that touches files, apps or destinations outside its approved plan is paused and flagged.
- EP-6. Ransomware recovery: IT can roll a machine's user data back to its last clean snapshot from the admin console, remotely. Snapshots can't be altered or deleted by user or agent processes.
- EP-7. Alerts and telemetry stream in OCSF into the customer's existing XDR console, such as CrowdStrike Falcon, Microsoft Defender XDR or Sentinel, or SentinelOne, through each vendor's third-party data ingestion.
- EP-8. A documented, stable sensor interface built on the audit system and DTrace, so an EDR vendor can port their agent to Glasshouse with minimal work.
- EP-9. Removable-device control: IT can allow, block or make read-only USB storage and other devices by policy.
- EP-10. Vulnerability tracking: every machine reports its installed packages and matching CVEs to the admin console, with an exportable software bill of materials.
- EP-11. Signing keys live in hardware security modules. Using them requires multiple people in a recorded key ceremony, with separate keys for the OS, packages and each release channel.
- EP-12. Builds are reproducible, so customer security teams can verify under NDA that a release matches its source. Update metadata is signed and supports key rotation and recovery from a compromised key without re-imaging the fleet.
- EP-13. The update and signing infrastructure is isolated from the rest of our systems and gets its own penetration test before every major release.
**Branding and open-source notices**
End users never see that Glasshouse is built on FreeBSD. The only exceptions are the legal notices open-source licenses require, and disclosure to customer security teams during due diligence.
- BR-1. No FreeBSD names, logos, mascot, boot messages or strings appear anywhere a user looks: installer, boot, login, settings, system information, error messages, default shell prompts and help. Every system-identity value, from the reported OS name and version to the kernel identity and package tools, says Glasshouse.
- BR-2. Open-source license notices (BSD, MIT, Apache, GPL and others) are reproduced as each license requires, in a Legal notices page under Settings and in product documentation. The BSD license requires its copyright notice to ship with binary distributions, which is why it can't be removed entirely. This is the same approach Apple and Sony take.
- BR-3. Components under the GPL, including the Linux kernel inside the developer workspace, come with the source-code offer the GPL requires.
- BR-4. Customer security teams get full base-system details and the complete software bill of materials under NDA during due diligence. The SBOM export (EP-10) is available to admins only, never published publicly.
- BR-5. Upstream contributions to FreeBSD can go through contractors or Foundation sponsorship, if keeping a low public profile matters.
**Building the base system**
Glasshouse builds on pkgbase, FreeBSD's packaged base system. It ships as a technology preview in FreeBSD 15 and is planned to replace the old freebsd-update method entirely in FreeBSD 16 ([OSnews](https://www.osnews.com/story/143942/freebsd-15-0-released-with-pkgbase/)). Building on it keeps our fork small and our security fixes fast.
- BS-1. A thin fork: our changes to FreeBSD's source are a small, documented patch set on top of FreeBSD's stable branch, rebased each release. Generic fixes go back upstream.
- BS-2. We rebuild only the base packages we change (kernel, branding, security configuration) and take FreeBSD's packages for the rest, re-signed into our own repository.
- BS-3. Each product image is a list of packages from that one repository: the desktop, the developer-workspace host, and later the cloud agent computer. Images include only what they need, with no compilers, test suites, debug tools or 32-bit libraries.
- BS-4. Devices receive whole signed images through ZFS boot environments (EP-1). Nothing is compiled on a user's machine.
- BS-5. Security pipeline: a FreeBSD advisory triggers an updated package, a rebuilt and tested image, and a release within the critical-fix commitment (CF-50).
- BS-6. Every component is a versioned package, which feeds the software inventory and advisory matching (EP-10).
**A thin platform layer**
Glasshouse's value lives above the kernel. We don't plan to replace FreeBSD, but we design so we're never trapped by it.
- PL-1. All FreeBSD-specific features (jails, Capsicum, ZFS, bhyve, pf and veriexec) are reached only through a thin platform adapter. Each has a documented Linux equivalent, such as namespaces with seccomp or Landlock, Btrfs or ZFS on Linux, KVM, nftables and dm-verity.
- PL-2. The brokers, model gateway, policy engine, desktop shell, agents, apps and admin console contain no FreeBSD-specific code outside the adapter. Our build pipeline also builds and tests these components on Linux.
- PL-3. We revisit the base only if FreeBSD laptop support stalls on hardware we need, EDR vendors refuse to support it and customers insist on their own agent, or a major laptop maker or buyer requires Linux.
- PL-4. We aim to become a significant FreeBSD contributor, which gives us influence over the platform instead of an exit from it (through contractors or sponsorship if a low profile matters, per BR-5).

## Data protection and model gateway
All model traffic leaves the machine through one gateway, which enforces data labels, redaction and the approved-model list before anything is sent.
**Model gateway**
- DP-1. Native agents can't reach model APIs directly. pf blocks it, and the gateway is the only path. External harnesses like Claude Code follow HX-7.
- DP-2. IT maintains an allowlist of models and providers, with Amazon Bedrock as the default (see Model providers below), plus routing rules by data label and task type.
- DP-3. The gateway redacts secrets, credentials and configured PII patterns before sending, and restores them in responses where safe.
- DP-4. Prompt-injection screening runs on retrieved content (web pages, emails, documents) before it reaches the agent. Flagged content is quarantined and shown to the user.
- DP-5. Every call is logged with the model, token count, cost, data labels touched and the task it served.
- DP-6. Per-user and per-team token budgets, with soft and hard limits set by IT.
**Model providers**
Amazon Bedrock is the default and first-class at launch, so enterprises can run agents on models inside their own AWS account. Teams can also bring their own Anthropic or OpenAI access, by API key or by signing in with an existing subscription.
| Access mode | How it connects | Who pays | Best for | Release |
| Amazon Bedrock (default) | Org's AWS account via IAM Identity Center or a cross-account role, optional PrivateLink | Org's AWS bill | Regulated enterprises already on AWS | P0 |
| Anthropic API | Org or team API key held by the secrets broker | Org's Anthropic account | Newest Claude models and features first | P0 |
| OpenAI API | Org or project API key held by the secrets broker | Org's OpenAI account | Teams standardized on OpenAI | P0 |
| Claude subscription | User signs in to Claude Code with their Claude plan. Our OS plugin lets Claude Code drive the machine (see below). | Per seat, on the user's Claude plan | Power users and teams already on Claude | P0 |
| ChatGPT subscription | Sign in with ChatGPT inside the OS, drawing on the plan's Work and Codex usage | Per seat, on the user's ChatGPT plan | Pilots, small teams, people who already pay | P0, after joining OpenAI's partner program |
| Local model | On device | Included | Confidential data, offline | P1 |

Azure OpenAI and Google Vertex AI follow later.
- MP-1. First-run setup offers Bedrock first. Admins connect an AWS account through IAM Identity Center or a cross-account role. No long-lived AWS keys ever sit on a device.
- MP-2. Bedrock calls can be pinned to a region for data residency, sent over PrivateLink, and appear in the org's own AWS bill and CloudTrail.
- MP-3. Bedrock Guardrails can run as an extra check after our gateway's checks.
- MP-4. Anthropic and OpenAI API keys live only in the secrets broker, scoped per team, with spend tracked per key and rotation from the admin console.
- MP-5. Bring your own subscription, the way each provider allows it: Claude plans through Claude Code itself (HX-1 to HX-7), ChatGPT plans through Sign in with ChatGPT. Credentials stay in the secrets broker or the harness's own login, never visible to other agents.
- MP-6. Admins choose which access modes each group may use. For example, Bedrock only for groups handling Confidential data, and personal subscriptions turned off in regulated orgs.
- MP-7. Agents are provider-neutral. The gateway exposes local Anthropic Messages and OpenAI-compatible endpoints, so third-party agents and tools work unchanged. Capability flags cover features that differ by provider.
- MP-8. Routing and failover by data label, task and cost. On an outage or rate limit, the gateway fails over to the next allowed provider and logs it.
- MP-9. The admin console's model catalog lists each model's provider, region, data terms and price.
**Claude Code and Codex as agents**
Claude Code is a first-class way to drive the OS. Users sign in to Claude Code with their own Claude plan, and our plugin gives it the skills and tools to operate the machine. Anthropic bills its Agent SDK separately from subscription limits ([Zed](https://zed.dev/blog/chatgpt-subscription-in-zed)), so Claude Code itself is the right path for subscribers. OpenAI took the opposite route: since September 29, 2026, Sign in with ChatGPT officially lets participating third-party tools draw on Plus and Pro plan usage, with per-app weekly caps ([The New Stack](https://thenewstack.io/sign-in-with-chatgpt/)).
- HX-1. We ship an OS plugin for Claude Code. Its skills teach Claude Code the OS: the action registry, recorded flows, approval tiers and how to hand control back to the user. Its MCP server exposes app actions, the accessibility tree and screen control (navigation layers 1 to 3).
- HX-2. Each Claude Code session runs inside an agent jail like any other agent. Its built-in shell and file tools see only what the task grants.
- HX-3. Plugin hooks report each step to the session's agent tile and route Claude Code's permission prompts into our approval tiers. Claude Code sessions get the same tile, take over, timeline and undo as native agents.
- HX-4. The OS ships managed Claude Code settings that IT controls: allowed tools, MCP servers and model backend (Claude plan, Anthropic API or Bedrock).
- HX-5. The same plugin works when Claude Code runs on Bedrock, so enterprises get Claude Code driving the OS with traffic kept in their AWS account.
- HX-6. The MCP server is harness-neutral. Codex and other MCP clients can drive the OS the same way, and Codex can run on a ChatGPT plan.
- HX-7. External harnesses send model traffic straight to their provider, not through our gateway. So the MCP server enforces data labels before returning any content, and egress is allowlisted to that harness's provider only. Confidential data is blocked for harnesses on personal subscriptions by default.
**Data labels**
| Label | Can go to | Example |
| Public | Any approved model | Marketing copy |
| Internal | Approved cloud models under enterprise terms | Team docs, forecasts |
| Confidential | Local model or a specific private deployment only | Customer records, financials |
| Restricted | No model. Agent can't open it. | Board materials, M&A, credentials |

- DP-10. Labels come from existing sources first: Microsoft Purview sensitivity labels, Google Drive labels, folder rules. Then they're stored as ZFS properties or extended attributes so they follow the file.
- DP-11. Content derived from a labeled source inherits the label, so a summary of a Confidential file stays Confidential.
**Local inference**
- DP-20. A small local model handles Confidential work and offline mode. It covers classification, redaction, summarizing and form-filling.
- DP-21. Open question: CUDA is Linux-only, so local GPU inference may run in a small Linux bhyve guest or on CPU or NPU at first. See Risks.
**Offline**
- DP-30. With no network, the desktop and apps work normally, agents limited to local models keep working, and cloud-dependent tasks queue with a clear notice.

## Audit, attribution and undo
Every agent action is recorded, labeled as agent work and reversible within the limits of what the OS controls. Undo is the feature that makes "reversible beats confirmable" real.
**Audit**
- AU-1. Each agent action logs the requesting user, agent identity, task ID, action, target, policy decision, approval (if any), model calls and snapshot ID.
- AU-2. Logs are append-only and hash-chained on device, then streamed in standard formats (OCSF and CEF) to the org's SIEM (Splunk, Microsoft Sentinel, Datadog) within 60 seconds when online.
- AU-3. Auditors can rebuild any task end to end: what was asked, the plan, each step, every approval and the final changes.
- AU-4. Users can view their own agents' logs. IT and security see the fleet, gated by role.
**Attribution**
- AU-10. Files, emails, records and messages an agent creates or edits carry "by [agent] for [user]" metadata. First-party apps show it inline.
- AU-11. Downstream systems see the agent sub-identity through on-behalf-of tokens (PA-4).
**Undo**
| What changed | How it's undone | Guarantee |
| Local files and settings | ZFS snapshot rollback for the task's datasets | Full |
| First-party app data | App-level transaction log | Full |
| Third-party apps exposing undo actions | Compensating action via the registry | Best effort, shown per item |
| Sent email, posted records, payments | Not reversible. That's why they sit in the Ask tier. | None, flagged before acting |

- AU-20. Snapshots are taken before every Notify-tier step and every 5 minutes of activity, kept for 30 days by default.
- AU-21. "Undo all" on a finished task reverses every reversible change and lists anything it couldn't undo.
- AU-22. Rolling back to a point mid-task restarts the agent from that snapshot only if the user asks.
- AU-30. Model-risk evidence pack: for each agent, a one-click export of its purpose, the models and versions it used, its permissions and approval tiers, test results, incidents and rollbacks, and performance over time. The pack is structured to support banks' model-risk documentation under Federal Reserve guidance SR 11-7.

## IT admin console and fleet management
One web console where IT sets agent policy, manages devices and proves compliance. The default policy for a new org is safe enough to ship unchanged. See the Admin · agent policy artboard on the [mockups canvas](https://claude.ai/artifact/Y5fH2vFk3fWUvWumZTRATC).
**Agent policy**
- AD-1. Policies apply per group, synced from Entra ID or Okta via SCIM.
- AD-2. Per agent: allowed apps and actions, the approval tier for each action class, allowed data labels, allowed models and egress destinations.
- AD-3. Policy changes are versioned, previewed ("this change affects 214 users and 3 agents") and rolled out in stages.
- AD-4. A live view shows running agents across the fleet, pending approvals and policy blocks in the last 24 hours.
- AD-5. An agent catalog where IT approves, versions and publishes agents and recorded flows to groups.
- AD-6. The admin console is its own web app, served from our servers. Admins sign in from a browser and see their fleet: devices, people, groups, policy, apps and agents, in the way Google Admin manages Chromebooks.
- AD-7. Which apps a person gets in their profile is policy, set by role or group. The admin UI and the IT tools for managing a computer appear on a device only for people whose role grants them, and they read from our servers. Everyone else never sees them.
**Fleet**
- AD-10. Zero-touch enrollment from the OEM or by USB image. Device joins the org on first sign-in.
- AD-11. OS and app updates in rings (pilot, early, broad) with automatic rollback through ZFS boot environments.
- AD-12. Remote lock and wipe, disk-encryption key escrow, device compliance status.
- AD-13. Integrates with existing tools where possible: compliance status to Intune or Jamf-style conditional access, logs to the SIEM, tickets to ServiceNow.
**Fitting the stack IT already runs**
- AD-14. Glasshouse agents register in Microsoft Agent 365 and appear on its agent registry and Agent Map, with their status, owners and policy blocks. Glasshouse never becomes a separate silo of agents.
- AD-15. IT can set Glasshouse agent policy from Intune and Entra, as well as from our console. Our console covers what those tools can't express yet, such as approval tiers, undo retention and data-label routing.
**Communications compliance**
- AD-16. Admins set up communications archiving and supervision in the admin console. They connect an archiving provider (such as Smarsh, Global Relay or Microsoft Purview), choose which agent-sent emails and messages are captured, and route them into supervisory review queues, the same way employee communications are handled under SEC and FINRA rules.
- AD-17. Admins can require pre-send review for chosen categories of agent communication, such as anything sent to a client.
**Compliance and reporting**
- AD-20. Reports by user, team and agent: tasks run, approvals, rollbacks, policy blocks, model spend.
- AD-21. Evidence exports mapped to SOC 2, ISO 27001 and financial-services audit requirements.
- AD-22. Prompt-fatigue report per team, with suggested tier changes (DA-14).

## Licensing and activation
A device can be licensed by an org admin, by the user directly, or by the hardware partner that built it. All three go through one activation service, so a device behaves the same however it was licensed.
[embed: licensing paths · admin, individual, hardware partner (node/5d1a0804-cdec)]
A lapsed license never locks anyone out: agents pause, but the desktop, data and security updates keep working.
**License model (proposed)**
- LA-1. A per-user subscription covers up to 3 devices. Per-device licenses cover shared machines and kiosks. Model usage is billed separately through the chosen provider (see Model providers).
- LA-2. Licenses are signed entitlements bound to the device's TPM and hardware ID, checked online at least every 30 days. Offline machines keep working through that window, and air-gapped sites use signed license files.
- LA-3. After a lapse, a 30-day grace period runs before agents pause. The desktop, apps, data and security updates keep working no matter what.
**Admins**
- LA-10. Admins buy seats self-serve or through sales and manage the license pool in the admin console.
- LA-11. Remote activation: admins assign licenses to users or devices from the console, by CSV or API, or automatically by Entra or Okta group. A device pre-registered to the org activates itself at first boot with no IT touch.
- LA-12. Admins can deactivate, reassign and reclaim licenses remotely. A license returns to the pool automatically when its user leaves the directory.
- LA-13. License state (active, grace, lapsed) shows in the console and flows into Intune compliance status.
**Individuals**
- LA-20. Anyone can start a 30-day trial or buy a license during first-run setup, with activation in under a minute.
- LA-21. A personal license can move into an org later. The org takes over billing and management, and the user's personal data stays theirs.
**Hardware partners**
- LA-30. Certified OEMs write a signed Glasshouse entitlement into each device's firmware at the factory, so it ships ready to activate. Windows OEM keys work the same way today.
- LA-31. When an enterprise buys through an OEM or reseller, the order's serial numbers are registered to the org in our partner portal. Devices then join the org and activate at first boot, like Windows Autopilot or Apple Business Manager.
- LA-32. A partner portal where OEMs and resellers buy license inventory, register devices to customers and track activations.
- LA-33. Repairs: when a motherboard or TPM is replaced, the partner transfers the license through the portal and the customer doesn't buy again.

## Developer program and catalog
Glasshouse needs a developer program and an IT-controlled catalog, not a consumer App Store. Because only signed software runs (EP-2), signing and distribution are required for anyone to ship anything. The third-party ecosystem centers on agents, app actions, adapters and recorded flows, which are small to build and portable across platforms, rather than native apps.
**Developer program**
- DV-1. Developer accounts are free. Publishers verify their identity to get signing certificates, and every app, agent, adapter, skill and recorded flow must be signed.
- DV-2. Every submission gets automated checks: malware scan, dependency CVE scan, and a test that its behavior matches its declared privileges. Low-risk items clear in under an hour.
- DV-3. Every listing declares its privileges: actions it calls, data labels it touches, network destinations and default approval tiers. The OS enforces the declaration, so an item can never exceed it.
- DV-4. A person reviews anything that can send, post, delete or move money, plus a random sample of everything else.
- DV-5. We can revoke a signature fleet-wide within an hour. Affected items are disabled and admins are notified.
- DV-6. SDKs for app actions, adapters (MCP), agents and recorded flows, with a local test sandbox that simulates policy and approval tiers.
**Each org's private catalog** (extends AD-5)
- DV-10. Each org has a private catalog in the admin console. Only approved items can be installed. Internal apps and agents signed with the org's own certificate never appear publicly.
- DV-11. Per group, each item is required (pushed silently), available (self-install) or blocked.
- DV-12. Version pinning and staged rollout rings, with automatic rollback if an update causes failures.
- DV-13. Only single-user apps built on a device no organization manages can go to the public store. Enterprise apps, and anything built on a managed device, stay in that organization's private catalog and can never be published.
**Public exchange**
- DV-20. A public exchange lists apps, agents, adapters, skills and recorded flows. Each listing shows the verified publisher, declared privileges, ratings and how many regulated orgs have approved it.
- DV-21. Existing MCP servers and Claude Code plugins can be listed by adding a manifest, with no rewrite.
- DV-22. A curated, signed collection of open-source packages, branded as Glasshouse packages. Apps running through Linux compatibility are marked best effort.
**Business model**
- DV-30. Listing is free with no revenue share at launch. Paid listings come later, billed through enterprise procurement the way AWS Marketplace works, with a low fee.

## Familiar desktop and app compatibility
A Windows user should feel at home in minutes, and every app the beachhead uses must run on day one, natively, in the browser or in a managed Windows session.
**Desktop**
- DK-1. The desktop is Mac-like: a menu bar across the top, a dock at the bottom, window controls on the left, and agent tiles at the top right. The command bar opens over everything with Ctrl Space, like Spotlight: typing finds apps by name and Enter opens the top one; anything else goes to the desktop agent (see the Desktop artboard).
- DK-2. Common Windows shortcuts work as expected (Alt Tab, Ctrl C, Win-key launcher equivalents).
- DK-3. AI is optional. A user who never opens the command bar has a complete, normal desktop.
- DK-4. Accessibility at WCAG 2.2 AA across the shell, with screen reader, high contrast and keyboard-only operation.
**App strategy**
| App type | How it runs | Agent access |
| Microsoft 365, Google Workspace, SaaS | Built-in Chromium-based browser with enterprise policy | Actions via adapters, else accessibility tree |
| First-party apps (files, mail, calendar, PDF) | Web apps on our engine (UI-5) | Full actions coverage |
| Teams, Zoom, Slack | Web versions, with native wrappers where needed | Actions via APIs |
| Legacy and Windows-only apps | Managed Windows VM, app streaming (Azure Virtual Desktop, Citrix) or Windows 365 for Agents | Screen control + recorded flows |
| Line-of-business web apps | Browser | Adapter SDK or screen control |

- DK-10. Before each deal, a compatibility assessment tool scans a sample of the customer's Windows fleet and lists each app's path.
- DK-11. Printing, scanning, docks, webcams and headsets work on certified hardware with no setup.
- DK-12. V1 ships on a short certified hardware list from one or two OEMs. Broad hardware support isn't a v1 goal.

## Desktop experience and design
Glasshouse should feel calm, fast and opinionated: great defaults out of the box, one switch to restyle everything, and an agent one click away from any problem. Some of these choices come from Omarchy, a popular agent-first Linux desktop that bills itself as a malleable OS for the age of agents ([omarchy.org](https://omarchy.org/)). It shows real demand for a desktop people can reshape with agents. Glasshouse's answer for the enterprise is malleable, but governed. See the Snapping and ask the agent and Themes artboards on the [mockups canvas](https://claude.ai/artifact/Y5fH2vFk3fWUvWumZTRATC).
**Defaults and setup**
- UX-1. Chef's-choice defaults: one curated set of apps, settings and layouts, so users face almost no upfront choices. IT policy covers the rest.
- UX-2. Setup asks individuals five questions or fewer and org-enrolled devices none. A fresh install reaches a working desktop in under 5 minutes.
- UX-3. A "Try Glasshouse" virtual machine runs on Mac and Windows, so evaluators and pilot teams can explore before installing.
**Themes**
- UX-10. One theme restyles the whole system at once: menu bar, dock, windows, tiles, notifications, terminal and wallpaper.
- UX-11. IT can set an org brand theme and choose which other themes users may pick. Every theme must meet WCAG 2.2 AA contrast, and light and dark modes work everywhere.
**Speed and the keyboard**
- UX-20. Everything is reachable by keyboard, with a shortcut cheat sheet one key away. Familiar Windows shortcuts work alongside the Mac-style ones.
- UX-21. Window snapping and saved layouts: drag a window to an edge or use the layout picker for side-by-side and grid layouts. An optional tiling mode serves power users.
- UX-22. Performance budgets are checked on every build for input lag, app launch, window open and wake to a usable desktop. A standing papercut program fixes small annoyances fast.
**An agent one click from any problem**
- UX-30. Every error, crash and failed-update notification offers "Ask the agent." The agent examines logs or crash data within the user's permissions, explains what happened in plain language, fixes what policy allows, and otherwise opens a help desk ticket with diagnostics attached.
- UX-31. Any setting can be explained ("why is this turned off?") and, within policy, changed by the agent. Admin policy can be exported as readable text, so it can be reviewed and versioned like code.
**A malleable desktop**
- UX-40. The desktop itself can be tailored. Menu-bar widgets, dock add-ons and shell extensions are catalog items that can be built on request (GA-1 to GA-11) or tailored with layers (TL-1 to TL-22), under the same review and permissions as everything else.

## Core OS and workplace features
These are the everyday basics enterprise buyers check before anything AI-related. Sign-in, networking, meetings and laptop reliability are where non-Windows desktops have historically lost enterprise deals, so they're treated as release gates.
**Sign-in and identity**
- CF-1. People sign in with their Entra ID or Okta account at the login screen, with cached credentials for offline sign-in.
- CF-2. Passwordless options: passkeys (FIDO2), fingerprint and face backed by the TPM, and smart cards (PIV and CAC) for government and finance.
- CF-3. Desktop single sign-on: signing in to Glasshouse signs the user in to the browser and apps through their identity provider.
**Corporate networking**
- CF-10. Enterprise Wi-Fi and wired authentication (802.1X), device certificates issued through SCEP or PKCS, and proxy auto-configuration.
- CF-11. A built-in VPN using open standards (IPsec IKEv2 and WireGuard) at launch. Vendor zero-trust clients such as Zscaler, Palo Alto GlobalProtect and Cisco Secure Client come through partnerships once a design partner needs them, the same path as EDR.
- CF-12. Settings has a Wi-Fi pane: nearby networks, joining with a password or the org's 802.1X (CF-10), and saved networks people can forget. Networks IT pushes join on their own and can't be removed, and IT can block networks. Certified laptops use Wi-Fi chips the base system supports (Intel iwlwifi, Realtek rtw88 and rtw89).
- CF-13. Settings has a Bluetooth pane: pair, see and remove keyboards, mice and other classic Bluetooth devices. IT can turn Bluetooth off or allow only input devices. Headsets and Bluetooth LE devices need base-system work first, which we fund or contribute as for sleep (PM-10).
**Browser**
- CF-15. We ship and maintain our own Chromium-based browser build, because the apps our customers depend on run through it. It supports Chrome enterprise policies and IT-managed extensions.
- CF-16. Browser security fixes ship within 3 days of an upstream Chrome security release, and within 48 hours for actively exploited vulnerabilities.
- CF-17. Every build passes a compatibility suite covering Teams, Zoom, Google Meet, Microsoft 365, Google Workspace, Salesforce, ServiceNow and Workday, including whether each service accepts our browser as supported.
**Meetings**
- CF-20. Teams, Zoom and Google Meet work fully in the browser: camera, microphone, noise suppression, background blur and screen sharing. Screen sharing is tested on every build and is a release gate.
- CF-21. Glasshouse meetings: video meetings for up to 25 people in the org, scheduled from Calendar like any invite and joined from the event. Camera, microphone and screen sharing, end-to-end encrypted, never recorded. Guests from outside the org come later.
**Laptop basics (release gates)**
- CF-30. Sleep, wake and battery life meet the Power management requirements below (PM-1 to PM-10).
- CF-32. Multiple monitors, docks and mixed-resolution scaling work without setup.
**Power management**
Sleep and battery life are the likeliest reasons a Glasshouse pilot fails, because FreeBSD's support for modern laptop sleep is still landing. The FreeBSD Foundation expects modern standby in FreeBSD 15.1 and hibernate possibly in 15.2 ([FreeBSD Foundation](https://freebsdfoundation.org/?p=29430)). As of mid-2026, work was still underway on NVMe resume and device power-state issues ([FreeBSD status report](https://reviews-dev.freebsd.org/R9%3Af16529a77e6a0aa12bfca3b50f6d57015f03e810?diff=1)).
- PM-1. Modern standby (suspend-to-idle and S0ix) works on every certified device. Closing the lid sleeps the machine, and opening it returns a usable desktop in under 2 seconds (proposed target).
- PM-2. Battery drain while asleep stays under 1% per hour (proposed target).
- PM-3. After a set time asleep, or when the battery runs low, the machine moves to hibernate with an encrypted hibernation image and resumes with everything as it was.
- PM-4. Every certified device passes 1,000 sleep and wake cycles with zero failures each release, including with docks, external monitors, USB4 and Thunderbolt, Wi-Fi and VPN reconnecting. This is a release gate.
- PM-5. Agents and sleep: local agents checkpoint and pause before sleep and resume on wake, with the tile showing "paused for sleep". Tasks on a remote runtime (PA-9) keep going. Agents never keep a laptop awake on battery unless policy allows.
- PM-6. Battery life within 15% of Windows on the same hardware, measured each release with a standard workload of browsing, documents and video calls (proposed target). Results are published.
- PM-7. Power modes (battery saver, balanced, performance) and a per-app view of power use. Background work such as indexing, sync and snapshots is deferred while on battery.
- PM-8. Local model inference is throttled on battery. Heavy local inference runs only when plugged in, by default.
- PM-9. IT sets power policy from the admin console (sleep timeout, hibernate, lid behavior) and sees battery health across the fleet for refresh planning.
- PM-10. A device is certified only after passing PM-1 to PM-6. We validate firmware with each OEM and fund or contribute upstream FreeBSD work on modern standby, hibernate, USB4 and Wi-Fi.
**Files, sync and recovery**
- CF-40. Built-in sync with OneDrive, SharePoint and Google Drive, with files downloaded on demand.
- CF-41. Device replacement: a new machine restores the user's files, settings and apps in under 30 minutes.
- CF-42. Users restore earlier versions of their own files from snapshots, without IT.
- CF-43. Reset and recovery: a recovery environment reinstalls or resets the machine without IT on site, keeping it enrolled in the org.
**Updates and support lifecycle**
- CF-50. A published support policy: how long each release is supported, and critical security fixes shipped within 48 hours.
**Employee privacy**
- CF-60. Users can always see their own agents' recordings and logs. Anyone else's access is role-gated and audited.
- CF-61. Retention is set by IT, with a short default for screen recordings and a longer one for structured logs.
- CF-62. An org-wide setting blocks any individual-level reporting from agent activity unless explicitly approved, so the data can't quietly become employee monitoring.
- CF-63. Agent screen recordings and logs are treated as sensitive data. They're encrypted with org-specific keys and take on the highest data label of anything they captured.
- CF-64. Legal hold: admins can place users, agents or date ranges on hold from the admin console. A hold overrides retention and blocks deletion until released.
- CF-65. eDiscovery export of recordings and logs in standard formats, with a chain-of-custody record.
**Desktop shell**
- CF-70. Universal search across files, apps, mail and agent history, respecting each source's permissions.
- CF-71. A calm notification layer: non-urgent notifications from every app and agent are batched into scheduled deliveries, urgent ones come straight through, and an agent can summarize what the user missed in any channel.
- CF-72. The clipboard respects data labels. Pasting Confidential content into an unapproved app or agent is blocked or warned, per policy.
- CF-73. US English at launch. Other languages, input methods and regions come later.
**Built-in remote help desk**
- CF-80. Remote help is part of the OS, with no separate tool to install. The tools a support person sees depend on their directory role when they sign in, mapped from Entra or Okta groups in the admin console.
- CF-81. Role tiers, configurable by IT. For example: help desk can view a screen and control it with consent; desktop support can also install approved apps, collect logs and restart services; security can isolate a device and collect forensic data.
- CF-82. The user approves each session and sees a visible banner while support is connected. Unattended access is allowed only for roles and device types IT permits. Every session is recorded in the audit log.
- CF-83. Support staff can see and manage the user's agents (pause, take over, undo) under the same consent rules.
- CF-84. Any elevated rights last only for the session and expire automatically.
**Working with people**
- CF-90. Users can hand agent work to a colleague: send flagged items, delegate an approval to a manager or request a review. These arrive in Teams or Slack, so colleagues who aren't on Glasshouse can act.
- CF-91. Glasshouse includes its own chat app: channels and direct messages for people in the org. Messages are end-to-end encrypted between the org's computers, so our servers store only ciphertext. IT holds a compliance key, so retention, legal hold and eDiscovery (CF-61 to CF-64) still work. Agents read chat without asking and post only with the person's yes. Teams and Slack still integrate through CF-20, CF-71 and CF-90.
- CF-92. Chat has the everyday features teams know from Slack: threads, reactions, @mentions that notify, editing and deleting your own messages, pinned messages, channel topics, browsing and joining public channels, unread markers, drafts and notification settings per channel.
- CF-93. Search across every conversation a person is in. It runs on their own computers, because our servers hold only ciphertext (CF-91).
- CF-94. Presence and a status (available, in a meeting, away or a custom line), set from Calendar and Do Not Disturb. A huddle starts a Glasshouse meeting (CF-21) from any channel or direct message in one click.
- CF-95. @-mentions of directory groups, and agents in channels: an agent can join a channel and answer when mentioned, posting with its owner's yes (CF-91). Docs, sheets and artifacts shared in a conversation show as cards (AR-5). Shared channels with other orgs come later.

## Developer workspace
Developers run unsigned code and Linux containers all day, which collides with signed-only software (EP-2) and a FreeBSD base. Glasshouse solves this the way macOS does with Docker Desktop: a managed Linux environment built into the OS, fenced off from corporate data and fully visible to security.
**For developers**
- DW-1. A developer workspace: a managed Linux VM on bhyve plus developer jails, where unsigned code may run. Docker, Podman and dev containers work as they do on a Mac.
- DW-2. Tools without a FreeBSD build, such as VS Code, JetBrains IDEs and some CLIs, run inside the Linux workspace transparently, with windows on the normal desktop.
- DW-3. Corporate root certificates reach every language runtime, container and VM. Package managers route through the company's mirror (Artifactory or Nexus), and SSO works for cloud CLIs.
- DW-4. SSH keys and commit-signing keys are stored in the TPM and can't be copied off the machine.
- DW-5. Claude Code, Codex and other coding agents run in the workspace with the same tiles, approvals and undo, through the harness plugin (HX-1 to HX-7).
- DW-6. Fast file sharing between the host and the workspace, and localhost networking so the host browser reaches services running in it. A higher-spec certified laptop tier for developers.
- DW-7. The Store offers developer tools to people with developer mode (DW-10): VS Code, git, the GitHub CLI, GitHub Desktop, and the core languages (Node.js, Go, Python, Rust, Java, .NET, Ruby, PHP, Swift and Zig). Each installs in one click from signed packages, several versions side by side, and a project's pinned version is honored (.nvmrc, go.mod, .python-version, rust-toolchain.toml).
- DW-8. Developer tools install into the person's developer jail (DW-1), never the signed base system, because what they build and download is unsigned (EP-2). Node versions come from our signed builds, since Node publishes no FreeBSD binaries; nvm itself, GitHub Desktop and other Linux-only tools run in the Linux workspace (DW-2).
- DW-9. The native VS Code is the open-source build, which installs extensions from Open VSX. Extensions Microsoft publishes only to its own marketplace, such as Pylance and Remote Development, need Microsoft's build in the Linux workspace.
**For security and IT**
- DW-10. Developer mode is granted by role: only members of a developer group in Entra or Okta get a workspace.
- DW-11. By default the workspace can't open Confidential or Restricted data, and its network access is limited to approved package sources, repos and cloud endpoints.
- DW-12. Packages must come through the company mirror, are scanned for known-malicious releases, and every workspace keeps a live software inventory.
- DW-13. No long-lived secrets on disk. Cloud and repo credentials come from the secrets broker as short-lived tokens, and secret scanning catches keys pasted into code or the clipboard.
- DW-14. Source code carries data labels, so it can't be pushed to personal accounts or sent to unapproved AI tools.
- DW-15. Platform teams publish versioned, approved workspace templates, so every developer starts from the same base.
- DW-16. Root inside the workspace is granted per session, expires automatically and is logged.
**Audit and visibility**
Security teams must be able to see and reconstruct everything that happens in a workspace, even when the developer has root inside it.
- DW-20. Every workspace streams activity into the Glasshouse audit trail and the customer's SIEM in OCSF: processes started and their commands, package installs with name, version and source, container images pulled and run, network connections and destinations, credential issuance and use, git pushes and where they went, root sessions, and coding-agent actions.
- DW-21. Anything crossing the workspace boundary is logged from the host side: files moved to or from the host, clipboard transfers and network traffic. A developer with root inside the VM can't turn this off.
- DW-22. The workspace is Linux, so the customer's existing EDR sensor (CrowdStrike, SentinelOne or Defender) can run inside it, closing the FreeBSD EDR gap for developers.
- DW-23. Tamper detection: if logging inside the workspace stops or is altered, the host isolates the workspace's network and alerts security.
- DW-24. Optional terminal session recording for chosen groups or repos, role-gated and audited like agent recordings (CF-60 to CF-64).
- DW-25. Investigation tools: security can freeze a workspace, take a forensic snapshot, cut its network and replay its timeline, from the admin console.
- DW-26. Reports on who holds developer mode, what packages entered the environment, unusual network destinations and secrets caught.

## Commercial, procurement and pilot readiness
Banks and insurers won't let a startup onto employee laptops without vendor due diligence, so pilot readiness is a product requirement, not a sales task.
**Passing due diligence during pilots**
- GT-1. SOC 2 Type I and a third-party penetration test are complete before the first pilot. The SOC 2 Type II observation period starts the same day, so the Type II report arrives during pilots.
- GT-2. Ready answers for standard security questionnaires (SIG Lite and CAIQ), a public trust center, and cyber and technology errors-and-omissions insurance before the first pilot.
- GT-3. Pilots keep customer data inside the customer's own boundary: model traffic runs through their Bedrock account and data stays on their devices. Our cloud holds only licensing, policy and telemetry metadata, which shrinks the scope of their review. What people share (chat, artifacts, docs) reaches it only as ciphertext it can't open. The one exception is Glasshouse Mail (EM): an org that chooses it keeps its mail in a mail server of its own on our AWS, which reads the mail to deliver, filter and serve it. It is offered only to orgs with no mail of their own, never to a regulated pilot, and the trust center says so.
- GT-4. Compliance automation from day one, so evidence for audits is collected continuously instead of rebuilt for each customer.
**Contracts**
- GT-10. Customers are responsible for what their agents do. Glasshouse provides the controls (approval tiers, the Never list, undo and audit), and our terms limit our liability to those controls working as documented. Legal writes this into customer terms before the first pilot.
**AWS Marketplace**
- GT-20. Glasshouse licenses are sold on AWS Marketplace as a SaaS contract, with private offers for negotiated deals, so customers can draw down their committed AWS spend. Only products with the "Deployed on AWS" badge count toward commitments since May 2025, and Marketplace purchases can cover up to 25% of a customer's annual commitment ([QuotaGuard](https://www.quotaguard.com/blog/aws-marketplace-static-ip-committed-spend)). AWS charges sellers a fee, typically 3 to 8% ([Redress Compliance](https://redresscompliance.com/aws-marketplace-vs-direct-procurement.html)).
- GT-21. Our control plane (activation, admin console, updates and the exchange) runs on AWS so the listing qualifies for the Deployed on AWS badge.
**Total cost of ownership**
- GT-30. A TCO model and calculator comparing Glasshouse against Windows with Microsoft 365 E5 and Copilot, EDR and VDI, including the Windows licensing still needed for our legacy-app fallback.
**Accessibility**
- GT-40. A published accessibility conformance report (VPAT) covering Section 508 and WCAG 2.2 AA before GA.

## Requirements by release
The prototype proves containment and the tile experience. Pilots prove a security team will sign off. V1 GA proves it works at fleet scale.
[embed: release roadmap · 4 phases, 3 gates (proposed) (node/83c84238-0a7d)]
Each phase starts only when the gate before it passes. Timing assumes a founding team of about 15 engineers and is a proposal, not a commitment.
**P0 · Prototype and pilots**
- Agent window: AW-1 to AW-5, AW-10 to AW-16, AW-20 to AW-22
- Delegation: DA-1 to DA-3, DA-10 to DA-13
- Navigation: NV-1 to NV-3, NV-10, NV-11, NV-20
- Platform: PA-1 to PA-6
- Data: DP-1 to DP-5, DP-10
- Audit and undo: AU-1 to AU-3, AU-10, AU-20, AU-21
- Admin: AD-1 to AD-7, AD-10 to AD-12
- Desktop: DK-1 to DK-3
- Model providers and harnesses: MP-1 to MP-9, HX-1 to HX-7
- Identity and integration: PA-7, AD-14, AD-15
- Endpoint protection: EP-1 to EP-3, EP-6, EP-7, EP-9
- Licensing: LA-1 to LA-3, LA-10 to LA-12, LA-20
- Developer program and catalog: DV-1 to DV-3, DV-5, DV-10, DV-11, DV-13
- Core OS: CF-1 to CF-3, CF-10, CF-20, CF-30, CF-32, CF-40, CF-50, CF-60 to CF-62, CF-80 to CF-82
- Power management: PM-1, PM-4, PM-5, PM-10
- Browser: CF-15 to CF-17
- Communications compliance and recordings: AD-16, CF-63, CF-64
- Signing infrastructure: EP-11 to EP-13
- Pilot readiness: GT-1 to GT-4, GT-10, GT-30
- Developer workspace for automation teams: DW-1, DW-3, DW-10, DW-11, DW-13, DW-20, DW-21, DW-23
- Branding and notices: BR-1 to BR-4
- UI stack: UI-1 to UI-6, UI-8, UI-9
- Agent engine: AE-1, AE-2 (invoice reconciliation), AE-3, AE-4, AE-10 to AE-12, AE-30
- Tailoring, minimal for Pilot 1: TL-1, TL-2, TL-10, TL-11 and TL-13 on our first-party agents, with undo
- Remix and share, the owner's focus from 2026-10-19: GA-1 to GA-14, TL-3 to TL-6, TL-20 to TL-22
- Desktop experience: UX-1, UX-2, UX-10, UX-20 to UX-22, UX-30
- Architecture commitments: FD-1 to FD-4
- Base system and portability: BS-1 to BS-6, PL-1, PL-2
- Working together (proposed 2026-10-20): AR-1 to AR-4, AR-5 inside the org, SK-1 to SK-3, DC-1 to DC-4, DC-6, SH-1 to SH-3, SC-1, SC-3, SC-4, IN-1 and IN-2 for mail and calendar, and EM-1 to EM-4 (moved up by the owner 2026-10-20)
- Chat, settings and developer tools (proposed 2026-10-20): CF-12, CF-92 to CF-95 (all of them, the owner 2026-10-20), DW-7 and DW-8 for tools with a native build, IN-5
**P1 · V1 GA**
- AW-6, DA-4, DA-14, NV-4, NV-21, NV-22, NV-30 to NV-32
- DP-6, DP-11, DP-20, DP-30, AU-4, AU-11, AU-22
- AD-13, AD-20 to AD-22, DK-4, DK-10 to DK-12
- Risk-based permissions and always-on agents: PA-8, PA-9
- Endpoint protection: EP-4, EP-5, EP-8, EP-10
- Licensing: LA-13, LA-21, LA-30 to LA-33
- Developer program and catalog: DV-4, DV-6, DV-12, DV-20 to DV-22
- Core OS: CF-11, CF-41 to CF-43, CF-70 to CF-73, CF-83, CF-84, CF-90
- Power management: PM-2, PM-3, PM-6 to PM-9
- Compliance and commercial: AD-17, AU-30, CF-65, GT-20, GT-21, GT-40
- Developer workspace, full: DW-2, DW-4 to DW-6, DW-12, DW-14 to DW-16, DW-22, DW-24 to DW-26
- Agent engine: AE-2 (remaining agents), AE-20 to AE-25
- Tailoring catalog items: TL-12, TL-14
- Desktop experience: UX-3, UX-11, UX-31, UX-40
- Working together (proposed 2026-10-20): AR-5 links outside the org, AR-6, SK-4 to SK-6, DC-5, SC-2, SC-5, IN-1 for chat and meetings, IN-3, IN-4
- Chat, settings and developer tools (proposed 2026-10-20): CF-13, DW-8 for Linux-only tools (GitHub Desktop among them), DW-9
**P2 · Later**
- Undo into third-party SaaS through compensating actions at scale
- A public agent and adapter marketplace
- Broad hardware support beyond the certified list
- Segment packs for call centers, healthcare revenue cycle and government case work
- Always-on cloud agents that work on the user's machine, in a managed Glasshouse cloud or the customer's AWS
- Paid listings through enterprise procurement (DV-30)

## Future direction: always-on agents with local hands
Once the OS is proven, agents move to the cloud so they're always on, while still working on the user's machine as if they were local. The agent's reasoning runs in the cloud. Its actions run on the laptop through the same brokers, approvals and audit as a local agent. When the laptop is asleep or offline, work continues on a cloud copy of the workspace and syncs back. This is not part of Pilot 1, but the architecture must not block it.
**Where it runs**
- A managed Glasshouse cloud for small teams and individuals.
- The same service inside the customer's own AWS for enterprises, since banks and insurers will expect always-on agents to stay within their boundary.
**Security rules for remote agents**
- A remote agent reaches the laptop only through the brokers, under exactly the same policy, approval tiers and audit as a local one.
- Its connection is bound to that specific device's hardware keys and the user's identity.
- It always appears as a tile on the user's desktop, and the kill switch stops it instantly.
**Design commitments now (P0, no user-facing feature)**
- FD-1. Brokers are location-independent: a call follows the same policy whether it comes from a local agent or a remote one.
- FD-2. An agent's work can be checkpointed and moved mid-task between laptop and cloud. The same mechanism handles sleep (PM-5).
- FD-3. Agent identity is never tied to one machine, building on directory-registered agent identities (PA-7).
- FD-4. Snapshots and undo history can follow a task across machines, so changes stay reversible wherever they were made.

## Risks, open questions and decisions needed
The biggest risks are distribution and hardware, not the agent technology. Microsoft already ships agent controls in Windows, so we win on depth and on fitting the Microsoft stack, not on being first.
**Risks**
| Risk | Impact | Mitigation |
| Microsoft has shipped agent containment and governance (Agent Workspace, Entra Agent ID, Agent 365) | Our main differentiator shrinks | Integrate with Entra Agent ID, Agent 365 and Intune instead of competing with them. Win on depth: whole-machine undo, data labels on every model call, provider neutrality with Bedrock. |
| FreeBSD laptop hardware, Wi-Fi and GPU gaps | Poor first impressions, slow certification | Short certified hardware list, OEM partnership, fund upstream driver work, Linux bhyve guest for GPU inference |
| App compatibility blocks deals | Lost sales at evaluation | Compatibility assessment tool before every deal. Windows VM and streaming as a backstop. |
| Prompt fatigue erodes the safety model | Users rubber-stamp approvals | Reversible-first tiers, task-level approvals, the prompts-per-day metric with admin nudges |
| Screen-control agents are unreliable on legacy apps | Failed tasks, lost trust | Recorded flows with replay first, agent only on failure. Honest "Failed" state with undo. |
| Enterprise sales cycles of 12 to 18 months | Runway pressure | Design partners on paid pilots, land with one team of 50 to 500 seats |
| A jail or broker escape | Breaks the core promise | Security audits, bug bounty, minimal broker code, memory-safe languages for new daemons |
| Major EDR vendors don't ship a FreeBSD sensor | Security teams can't see Glasshouse in their EDR console, which blocks regulated deals | Built-in protection (EP-1 to EP-6), alerts streamed into their XDR (EP-7), and a sensor interface ready for a vendor port (EP-8). Start a vendor partnership once a design partner needs it. |

**Open questions**
- Product name.
- Ship our own hardware with an OEM partner, or certify existing business laptops?
- Local inference: Linux bhyve guest with GPU passthrough, NPU, or CPU-only for v1?
- Pricing: per seat per month including model usage, or seat plus pass-through model costs?
- Which design partners: a bank AP team, an insurer's claims team, or both?
- Language for new daemons and the shell: Rust for brokers and the gateway is the default. Confirm for the UI toolkit.
- Open-source strategy: which layers are open (base, brokers) versus commercial (console, gateway)?
- How deep does undo go into third-party SaaS apps for v1?
- Join OpenAI's Sign in with ChatGPT partner program, and confirm whether Business and Enterprise plans can use it, not only Plus and Pro.
- Confirm with Anthropic whether Claude Code sessions the OS starts in the background count as subscription use or as separately billed Agent SDK usage.
- Which EDR vendor to approach first for a FreeBSD sensor port, and which design partner will sponsor the request?
- License model: confirm per-user (up to 3 devices) plus per-device for shared machines, and which OEMs to certify first.
- Fee for paid listings once DV-30 launches, and whether internal-only items ever carry a fee.
- Support lifecycle: how many years each release is supported, and the battery-life target versus Windows.
- Confirm the first two Pilot 1 workflows with design partners, and the accuracy bar each agent must clear.
- Hosted mail (EM): run our own mail servers on AWS, or a mail partner behind our console? Deliverability and abuse handling are an operations commitment. Decided 2026-10-20 (ADR 0037): our own, one mail server per org on our AWS, sending through Amazon SES, with our own delivery and abuse on-call.
- Links outside the org (AR-5, SC-1 booking pages) are public pages on our servers. Which orgs may turn them on, and with what review?
- Microsoft 365 (IN): Teams chat messages need Microsoft's approval for protected APIs. Until then, is Teams chat read-only, or opened in its web app?
**Decisions already made**
- Build on FreeBSD rather than writing a kernel.
- The agent is a first-class principal with its own identity, sandbox and audit.
- Visible tiles for every running agent, with take over and rewind.
- Structured actions first, screen control as fallback.
- Build on pkgbase with a thin fork. Never replace the kernel; keep Glasshouse portable above it through a thin platform layer.
- Our web apps ship their TypeScript source on customers' computers so people can remix them, under a license that allows changes for the customer's own use and no redistribution. Suggestions to us are off unless IT turns them on.
- We build our own Docs and Sheets with AI built in, which replaces NV-3's earlier rule of building no editors (2026-10-20). Artifacts, skills, docs and sheets live in one sealed shared space. Microsoft 365 can be the provider for mail, calendar, chat and meetings, and orgs without mail get Glasshouse Mail.
- Amazon Bedrock is the default model provider at launch. Claude subscribers drive the OS through Claude Code plus our plugin, and ChatGPT subscribers through Sign in with ChatGPT.
