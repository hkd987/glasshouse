# 0034. Skills: shared items with versions, evals and proposals

Status: **Accepted** (2026-10-20). PRD SK-1 to SK-6, and AE-1, TL-1 and
GA-8 where they name skills. Builds on ADR 0033 (the shared space), 0018
(the model gateway), 0010 (agents are harness bundles) and 0032 (remix and
share).

## Context
- A skill is the instructions, examples and files that teach an agent a
  task (AE-1). It is how an org's know-how reaches its agents. The owner
  wants skills shared simply, versioned, evaluated, announced, and improved
  by anyone who can show their change scores better.
- The PRD puts skills in the sealed shared space with artifacts, docs and
  sheets (2026-10-20). So a skill's content never sits readable on our
  servers (GT-3).
- Two things the platform lacks today shape this design:
  - an app can't start a task: the app socket opens sessions and calls
    actions only;
  - data labels come only from files' paths, so nothing read from the
    shared space raises a task's label.
- Chat's membership is fixed when a conversation is made (slice C1).
  Joining later waits for slice C2.

## Decision
1. **A skill is a shared-space item of kind `skill`** (ADR 0033 §1).
   - Its members are who may use it and propose changes.
   - Its sealed name is the skill's name.
   - Its maker owns it. Only the owner's `Version` messages count, as for
     artifacts.
2. **A version is a sealed bundle.** `Part::Version` with `mime`
   `application/vnd.glasshouse.skill+json`. Each kind reads only its own
   mime, so artifacts and skills never take each other's versions.
   - The blob is `{ "files": { "<path>": "<text>" } }`: text only, at
     most `MAX_BLOB_BYTES` in all.
   - `SKILL.md` is required. Its front matter names and describes the
     skill.
   - `evals.json` holds its evals (§3).
   - Paths come from whoever shares or proposes, so they are checked at
     share and propose, not later:
     - relative;
     - no `..` and no dot files;
     - plain characters;
     - at most 200 characters and 200 files.
3. **Evals travel with the skill.** `evals.json` holds cases:
   - each case has an `id`, a `prompt`, optional `files` (text the case
     gives the model) and `graders`;
   - deterministic graders are `contains`, `not_contains`, `equals`,
     `matches` (a glob: `*` and `?`, so no regex engine joins the build)
     and `json_path`;
   - `rubric` is graded by a second model call against the criteria it
     gives;
   - each case runs `runs` times (default 3, at most 5) and passes on a
     majority;
   - caps keep a run's cost known: 50 cases, 5 runs, and `max_tokens` of
     4,096 per call.

   A version's score is the share of cases passed. It is recorded with:
   - the model that answered (from `model.complete`'s result);
   - the settings;
   - the eval runner's version;
   - whose computer ran it.
4. **Evals run as a task, in a jail, through the gateway** (SK-5). The
   built-in `skill-evals` agent is a jailed SDK binary, like the IT agent,
   allowed `skill.read`, `model.complete` and `skill.results`.
   - For each case it calls `model.complete`. The skill's `SKILL.md` and
     files are the system prompt, and the case's prompt and files are the
     message. Then it grades the answer.
   - It reports with `skill.results`, which glassd keeps on this
     computer, per person. That is local and reversible, so Auto.
   - Model calls go through the pipeline, so the task's label decides the
     route and redaction, and each call is audited. The skills provider
     never calls the gateway itself: apps can't depend on services, and a
     direct call would skip routing.
   - The runner knows only what the case gives the model. A skill's
     scripts don't run in evals yet; that waits for evals that drive a
     whole harness (K3).
5. **Items carry a label** (DP; this also closes the gap for artifacts).
   - A share sets it to the higher of the sharer's choice and the label of
     the data the task has read. glassd passes that label in the task
     context.
   - The label is kept in two places:
     - in the clear on the conversation record, for IT's inventory;
     - sealed with the name, which is what computers trust. A computer
       refuses an item whose two copies differ.
   - An action that reads an item names the item's label in its preview,
     beside the paths it reads. The pipeline raises the task's label to
     it (DP-11), so a confidential skill's evals route as confidential.
6. **Anyone in it can propose a change** (SK-3).
   - `Part::Proposal { blob, key, sha256, bytes, base, before, after }`
     carries the proposed bundle and two score sets: the base version's
     and the proposal's.
   - Both sets come from one run, on one computer, with one model, or the
     comparison means nothing. `skill.propose` refuses without them. The
     Skills app runs the evals first.
   - The message's text says what changed and why.
7. **The owner merges.** A merge is the owner's next `Version`, with
   `merged` naming the proposal.
   - When a case that passed on the base fails on the proposal, the merge
     needs a `reason`, kept in the message (SK-3).
   - The proposer's scores are a claim. The owner's computer can run the
     evals again, and the scores shown with a version are the run its
     owner kept.
8. **Actions** (crate `glass-app-skills`):

   | Action | Tier | Notes |
   |---|---|---|
   | `skill.list` | Auto | Skills shared with the person, and their own |
   | `skill.read` | Auto | A version's or proposal's files, evals, scores and proposals; raises the label |
   | `skill.share` | Ask | Irreversible: others get it |
   | `skill.propose` | Ask | Irreversible |
   | `skill.merge` | Ask | Owner only |
   | `skill.results` | Auto | Local: an eval run's results, kept on this computer |

   - The Skills app's `[[app]]` tiers make the person's own share,
     propose and merge Auto.
   - Choosing which skills one's agents use is the person's own act in
     the app, never an agent action.
9. **Apps may start the agents their manifest names** (new: `app.start`).
   - It exists for the Skills app to run `skill-evals` for the person.
   - A manifest lists `agents`. `app.start` starts only those agents,
     with no folders, as the person, attributed "by <app> for <person>".
     It follows the same profile, policy and audit as a start from the
     desktop.
   - Until it lands, the person starts the evals from the desktop.
10. **Agents use pinned versions** (SK-6).
    - The person picks skills and versions in the Skills app. A task's
      skills are those pins, plus what a tailoring layer adds (TL-1).
    - glassd, as root, writes the opened versions into the task's sandbox
      folder. They are mounted read-only into the jail at the harness's
      skills path: `skills/` in a bundle (ADR 0010), and
      `~/.claude/skills/<name>/` for Claude Code.
    - The task's audit record names each skill and version.
    - The mount goes when the task does.
    - A new probe check proves it read-only and holding only the pinned
      versions.
    - A newer version that scores better is announced, never swapped in
      (SK-4).
11. **The catalog lists skills by the owner's choice** (SK-1, SK-4). A
    listing is clear metadata the owner publishes to the org's catalog,
    the ADR 0033 §3 pattern:
    - name and one line;
    - owner and version;
    - score and contributors.

    Content stays sealed. Joining a skill from its listing means a member's
    computer adds the person, which is chat's membership change (C2).

## Consequences
- Skills reuse the shared space's sealing, sync, retention and audit. The
  console holds no skill content, so it can't score, search or rank them.
  Computers do all three.
- A run's cost is bounded and visible before it starts: cases × runs ×
  (1 + rubric graders) model calls.
- `app.start` is a new runtime surface. Apps start only agents their
  manifest names, and the review of a remixed app's manifest (ADR 0032)
  shows a new one.
- Item labels make the shared space part of data protection. They also
  change artifacts, which take the label of the task that shared them.

## Slices
- **K1:**
  - item labels (§5, artifacts included);
  - the bundle and eval formats;
  - `glass-app-skills` with list, read, share, propose, merge and
    results;
  - the `skill-evals` agent;
  - tests on the scripted model provider: a proposal that fixes a
    failing case scores higher, one that breaks a case can't merge
    without a reason, and a confidential skill's evals route as
    confidential.

  Members are fixed at share.
- **K2:**
  - the Skills app;
  - `app.start`;
  - pinned skills mounted into agents' jails, with the probe check;
  - notices of better versions.
- **K3 (needs chat C2):**
  - the catalog listing and joining from it;
  - usage counts;
  - evals that drive a whole harness, scripts included.

## Amended by ADR 0052
A version counts only if it is the next number, its proposal was made on
the version before it (number and SHA-256), and that proposal wasn't
merged already; merge refuses a base whose SHA-256 differs. Two merges at
once no longer lose a change (`specs/tla/SkillMerge.tla`).
