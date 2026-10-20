# 0032. Remix and share: every app is source, every change is checked, the store is the only way between people

Status: **Accepted** (2026-10-19, the owner's decisions below). Slice R1
is built (below, "What R1 built"). PRD GA-1 to GA-11, TL-3, TL-4, TL-10, TL-20 to TL-22, DV-2
to DV-4, DV-10, UX-40. The owner asked (2026-10-19) that every piece of
software on the computer be hackable and customizable, that people share
what they make, that it never become shadow IT because it goes back into
the store and is vetted, and that an agent asked for software checks the
store first, then customizes the closest thing, and only then builds.
Builds on ADRs 0013, 0014, 0023, 0024 and 0026.

## Context
- First-party apps are TypeScript web apps (ADR 0009): Mail, Calendar,
  Chat, Meet, Settings and the Store, each a `manifest.json` and a
  bundle. An app reaches the system only through the actions its manifest
  declares, run by glassd through the agent pipeline under the person's
  policy (ADR 0024). `AppManifest::check` refuses any action that isn't in
  glassd's registry.
- So an app's code can change everything about its screens and logic and
  nothing about what it can reach. That is what lets "hackable" and
  "vetted" hold at once.
- Tier lowerings an app gets from policy (`[[app]]`, such as Chat's posts
  at Auto) are keyed by the app's id (`Policy::for_app`).
- Tailoring layers exist for agents (ADR 0014: personal, versioned,
  narrow-only). Copies (TL-4), team layers (TL-3) and suggestions (TL-20 to
  TL-22) don't.
- The catalog lists items IT approved, per group (DV-10, DV-11), and
  `catalog.search` finds them by need (GA-2). Items carry no software yet:
  a release says what an item is, not its bytes. Submissions come with the
  checks the submitter reports; nothing re-runs them (DV-2).
- The image has no TypeScript toolchain. Agents that work through actions
  (Claude Code, attended) can't run programs on the computer.

## Decision
1. **Every web app ships its source.** Images carry each first-party app's
   `src/`, its manifest and the SDK's source, read-only, beside its
   bundle. Catalog apps carry theirs in their submission. Native apps
   (Terminal, Activity Monitor) and the shell aren't remixed on the
   computer. They extend through web add-ons (UX-40) and layers. Agents
   keep layers (ADR 0014); their copies and team layers come later.
2. **A remix is a new app with a past.** It has its own id, so its own
   origin, and it records what it is based on (`mail@0.1.0`) and its diff
   from that. It is the person's (scope Personal, GA-5). Its manifest may
   declare only actions that exist and that the person's policy doesn't
   make Never, and labels within theirs. It never inherits the base app's
   `[[app]]` tiers: it asks where the person would be asked. Doing
   something new means a new action, which is a change to the OS (GA-11,
   the `add-action` skill), reviewed like any.
3. **glassd checks, builds and keeps every version.** Agents send source
   as action input, never as paths, so this works for an agent anywhere
   (FD-1). New app crate `glass-app-studio`:
   - `apps.bases` (Auto, reads): what can be remixed for a need: built-in
     apps, catalog apps with source, the person's own. Ranked as
     `catalog.search` ranks.
   - `apps.source` (Auto, reads): a base's files, manifest and version.
   - `apps.save` (Notify): checks, bundles and saves a new version.
   - `apps.versions` (Auto) and `apps.revert` (Notify): undo is choosing
     an earlier version; history is never edited (as layers, TL-2, GA-3).
   - The checks run before a version exists (GA-4), and their findings
     are the tool result, so the agent fixes and saves again:
     - The manifest, as `AppManifest::check` and point 2 have it.
     - Imports: only the app's own files and the SDK, read from the
       bundler's list of inputs, not from patterns in the source.
     - The app's own files: no `eval`, `new Function`, dynamic import,
       HTML strings (`innerHTML` and the like; text only, as every app),
       or raw network APIs. Action names are literal, and every action
       called is declared. Declared but never called is reported.
     - Limits on files, sizes and total source.
     - The bundle: esbuild (MIT), the version and checksum the apps'
       lockfile pins, so the computer builds people's apps as we build
       ours. It runs once per save in a jail of its own: the build
       folder is its whole filesystem, with esbuild's folder mounted
       read-only, no devices, no addresses, as nobody, for a minute at
       most, gone with its mount when esbuild exits.
   - Versions live in `<state>/apps/<login>/<id>/<n>/`, root-only. The
     person's bridge gets their apps' files from glassd over the app
     socket, so what runs is what was checked.
4. **A remix is only its creator's.** It appears in their launcher
   (`apps.list`) and in the Store under Yours, marked "Your version of
   Mail · based on Mail 0.1.0". Nobody else can open it. When its base
   gets a new version the remix stays as it is (TL-4) and says so. Asked
   to bring the changes over, the agent re-applies the diff to the new
   source and saves through the same checks.
5. **The store is the only way between people.** `apps.share` (Ask)
   submits a version to the console. It carries the source, manifest,
   provenance (base and base version), the diff from its base and the
   computer's check results. The console runs the checks again itself
   (DV-2) and shows the reviewer:
   - the diff;
   - the privilege delta: what it declares beyond its base, or
     everything for a new app;
   - who made it, from what.

   Routes:
   - Team: IT or the team's delegated owner approves (GA-8).
   - Catalog: IT approves. A person reviews anything that can send,
     post, delete or move money (DV-4).

   Approved, it becomes an org item, signed with the org's key. The
   catalog release carries its bundle's hash. Computers fetch the bundle
   and check the hash before serving it, which is how catalog web apps
   reach computers at all (DV-10). Declined, it stays personal, with the
   reviewer's note.
6. **Good changes flow back** (TL-20 to TL-22). `apps.suggest` sends a
   remix's diff to its base's owner:
   - For an org item, that is the owning team, in the console. Accepting
     it makes the item's next version, like merging a pull request.
   - For a built-in app, that is the org's IT. Accepting it makes the
     org's edition of the app. An edition replaces the built-in under
     its id, and so keeps its `[[app]]` tiers, so it always gets the
     full review.
   - IT may also forward a suggestion to us as product feedback. This
     is off unless the org turns it on, and sends the diff only.

   Owners see what people keep changing ("12 remixes of Mail, 7 add an
   unread count", TL-21). This comes from diffs and descriptions, never
   from app data.
7. **Store first, remix second, build third, enforced.** For an app with
   no base, `apps.save` needs the need it meets. glassd searches the
   catalog and the bases for it. If something the person may install or
   remix matches, the save is refused unless the input names what the
   person turned down. That first save then asks, saying so ("Acme's
   catalog has Dispute tracker; you chose to build your own"). The agent
   flow is:
   1. `catalog.search`, and suggest installing the match;
   2. `apps.bases`, then remix the closest;
   3. otherwise build from the SDK's template;
   4. `apps.save`, open it, offer to share.
8. **Not shadow IT: everything is in the inventory.** Each computer's
   report lists every personal app and remix: id, name, owner, base,
   version, declared actions, last opened. The console shows them all
   (GA-10). Policy sets who may remix and build, the labels such apps may
   use and how far they may be shared. An app unused for 90 days is
   retired after its owner is told, and ownership moves when someone
   leaves (GA-9).
9. **The Store is where the culture shows.** It has:
   - a "Made at Acme" shelf, with each maker's name;
   - Remix on every app;
   - lineage ("based on Mail · 4 remixes");
   - installs, and what changed between versions.

   The console lists popular remixes, so IT can adopt one as supported
   (GA-11).

## What R1 built
- `glass-app-studio` (`crates/apps/studio`):
  - the checks (`checks.rs`), read from tokens, not text, with
    comments skipped and regular expressions read whole;
  - the store (`store.rs`);
  - the bundler (`bundle.rs`);
  - the six actions.
- `Sandbox::run_tool` (`glass-platform`): the jail above, or a plain
  process on the development host.
- The runtime:
  - lists a person's apps after the org's;
  - opens one only for its maker, and a remix only while the app it
    remixes is in their profile;
  - closes one when IT takes that app away;
  - answers `app.files`.
- Policy never gives an `my-` id an `[[app]]`'s rules and refuses to
  name one. So do catalog releases and installed apps.
- The bridge serves `my-` origins from `app.files`, asking again with
  each page.
- Images carry our apps' source in a folder of its own
  (`/usr/local/share/glasshouse/source`: `apps/<app>/src`, and `sdk`
  with its template) and esbuild.
- TL-6: the device setting `apps.remix` (everyone's, on by default).
  Off, glassd removes the source folder (links removed, never followed),
  the studio refuses, and people's own apps leave their launchers and
  close; their versions stay in glassd's store, so on again brings them
  back. The source returns with the next update.
- Every computer's report lists what was made there. The console's
  Devices page shows it as "Apps people made".
- Tests:
  - the studio's own tests;
  - the runtime's `tests/studio.rs`;
  - the console's `ga10_…`;
  - `apps/e2e/studio.spec.ts`, where Claude Code remixes Mail over MCP
    and it runs on its own origin;
  - the VM scenario `studio`: a jailed bundle in 2.0 s, nothing left
    behind;
  - the probe's new check that agents can't open the store.
- The bundler's jail gets the rctl(8) limits an agent's task gets
  (memory, processes, CPU, disk), removed when it exits.
- A remix undone and made again goes on where it was, with the same id and
  the next version number.
- Not yet:
  - policy's say over who may build (GA-10; everyone may);
  - Meet can't be remixed until workers are allowed;
  - the Store's Yours shelf;
  - the remix's own window title, which comes from its source.

## R2, as built: sharing back to the store
- **The device's side.** `apps.share` (Ask; it sends the app's source
  off the computer) submits one of the person's own apps to the
  console, for their team (a directory group) or the whole org. The
  submission carries:
  - the source;
  - the manifest;
  - what it remixes, at which version;
  - the maker's note;
  - the computer's check notes.

  An unmanaged computer has nowhere to share to and says so.
- **Nothing is taken on trust.** The console runs the same checks
  (`glass-app-studio::checks`, against the same action registry) and
  rebuilds the bundle itself. The submitted app's bundle is the
  console's, not the computer's.
- **The console's confinement.** The console bundles with the same
  pinned esbuild, confined the way a jail confines it on a computer.
  On our Linux servers:
  - `prlimit` caps memory, CPU time and files;
  - `unshare` gives it user, mount, network, IPC and PID namespaces;
  - `--root` makes the build folder its root, with esbuild copied in.

  There is no network and nothing outside the folder. These are the
  base system's own commands, as ADR 0007 asks. A console that can't
  confine refuses to build rather than building unconfined.
- **The reviewer sees:**
  - the change from what it remixes, file by file (the console holds
    our apps' source, so the base it compares against is ours, never
    the computer's word for it);
  - the privilege delta: actions beyond the base, or everything for a
    new app;
  - every check's result;
  - who made it, from what and on which computer.

  An app that declares anything irreversible (sends, posts, deletes,
  money: `SideEffect::Irreversible` in the registry) is marked "a
  person must review this" (DV-4). Approving it is that review.
- **Approved, it is an org app.** Its id is its own (never `my-`). It
  joins the signed catalog release as an item that carries the app:
  its manifest, the bundle's SHA-256, the maker and what it remixes.
  Computers fetch the bundle from the console, check its hash against
  the signed release, and keep it root-only. glassd serves it like a
  person's own app (`app.files`), so what runs is what was signed. It
  reaches people by the catalog's per-group rules (DV-11): required,
  or available and installed from the Store.
- **Names, ids and versions.**
  - A share may name the app for the Store ("Mail, with Write"); by
    default it keeps its own name. The console refuses a name another
    app in the store, or one of ours, already has.
  - Its catalog id comes from the name, never `my-`, and no catalog
    item, app of ours or agent in policy has it.
  - The same person sharing the same app again makes the next version
    of the same item. It waits for review while computers keep the
    approved version.
  - Its maker must be in the directory; a team share must go to a group
    they are in.
- **What the console keeps.** Beside the catalog, per version: the
  source, the console's bundle and the change from the base. It serves
  a bundle only at the version the signed catalog gives computers.
- **The device's word for who made it.** Devices share one enrollment
  token for now (ADR 0023), so the console takes a computer's word for
  its user. Device keys (ADR 0030, the chat pilot gate) will sign
  submissions.
- **Team owners.** IT approves both team and org shares until
  directory groups have owners; GA-8's delegated approval waits on that.

## Slices
- **R1, remix on the computer:**
  - the source in the image, and esbuild;
  - `glass-app-studio` and its checks;
  - versions and undo;
  - personal apps served on their own origin and in the launcher;
  - the inventory in the device report.
  - An end-to-end test: an agent remixes Mail and the remix runs. An
    over-declared manifest, an unlisted import and an `innerHTML` are
    refused.
- **R2, share:**
  - `apps.share`;
  - console submissions, with the checks run again;
  - the review page, with the diff and the privilege delta;
  - team and catalog approval;
  - signed bundles delivered and checked;
  - a colleague installs it from the Store.
- **R3, flow back:**
  - `apps.suggest`;
  - accepting a suggestion as the next version or an org edition;
  - the aggregates;
  - the Store's culture surfaces;
  - retirement and the admin controls.
- **R4, new apps' own data** (GA-6): app data in org storage, with
  labels, retention and legal hold, behind actions. Until then a new
  app can only work through existing actions.

## Owner decisions (2026-10-19)
1. Our web apps' source ships on customers' computers for remixing, under
   a license that allows changes for the customer's own use and no
   redistribution.
2. Everyone may remix by default. IT can turn remixing off for the org
   (TL-6). Off, the apps' source is removed from its computers, the
   studio refuses, and people's remixes stop opening until it is on again;
   the source comes back with the next update. The source therefore lives
   in a folder of its own (`/usr/local/share/glasshouse/source`) that
   glassd can remove, apart from what the apps run.
3. Suggestions to us are off unless IT turns them on.

## Consequences
- An app's power is its actions. Remixing is safe to offer everyone
  because a remix can never reach more than its creator. The hard reviews
  are of new actions and of editions that keep a built-in's tiers.
- The image grows by esbuild (about 10 MB) and the apps' source (about
  100 KB).
- The console becomes a code review tool for small apps. Its checks must
  be the same code as the computer's, so they can't drift.
- Our own apps become what customers start from, so their source must
  stay readable: it is now product.
