# 0023. The admin console: a web app on our servers, signed releases, devices that sync in the background

Status: **Accepted** for the first slice (policy, catalog and fleet pages;
signing; device sync; profiles by role; `catalog.search`). PRD AD-1 …
AD-7, AD-10, AD-11, DV-1, DV-3, DV-10, DV-11, DV-13, GA-2, EP-12. Builds
on ADR 0021 (managed settings load from disk), ADR 0008 (declared
privileges) and ADR 0024 (apps share the agents' id namespace).

## Context
IT needs one web console to set agent policy per group (AD-1, AD-2),
version, preview and stage it (AD-3), approve and publish catalog items
per group (AD-5, DV-10, DV-11), and see the fleet (AD-4). The product
owner set its shape (AD-6): its own web interface, served from our web
servers, where admins sign in and see devices, users, policy and apps, as
Google Admin manages Chromebooks. Which apps a person gets is policy by
role, and IT's tools appear only for the roles that grant them (AD-7).
Devices must apply what IT publishes without trusting the network in
between (DV-1, EP-12), keep booting from disk and keep working offline
(ADR 0021). Agents building an app must first look for an approved one
(GA-2). And an org's apps never go public (DV-13).

## Decision

### Where it lives
- **The server is `glass-console`** (`crates/bin/glass-console`), a
  binary in the main workspace that never ships in the OS image (it is
  not in `dist.manifest`). It serves the web app, the admin API
  (`/api/…`) and the device API (`/device/v1/…`) with the small HTTP/1.1
  server the app bridge already used, now the lib crate `glass-http`
  (tokio and httparse, already in the tree). It reaches the rest only
  through lib and svc crates: `glass-managed` (the contract),
  `glass-policy` (the same validation and lint devices run, so it never
  signs a policy a device would reject) and `glass-first-party` (action
  titles and lint information).
- **One mode is built, `--dev`**, like `glass-app-bridge --dev`: loopback
  HTTP, state as JSON files in a folder, sign-in by picking one of the
  directory's admins. Production adds TLS at our web servers, the org's
  SSO and a managed database (GT-21) in front of the same code; none of
  that is built.
- **The web app is `apps/console`**, built by `apps/build.mjs` as a *site*
  (no offline service worker: it reads from its server) on the apps' SDK,
  whose DOM builder inserts text only. Its Playwright suite is
  `apps/e2e/console.config.ts`, on the apps' Playwright install and
  browsers. `make console-dev`, `console-test`, `console-lint`.
- **`glass-managed`** (`crates/lib/managed`) is the contract, as
  `glass-api` is for the shell: the signed envelope, catalog items with
  their declared privileges, the store rule, catalog-first search, sync
  and report messages, and the device's store of applied releases.

### The console
- **Groups come from a `Directory`** (`glass-console/src/directory.rs`):
  today `directory.toml` in the state folder, later SCIM from Entra ID or
  Okta (AD-1) behind the same trait. Roles are groups: the members of
  `it-admins` sign in to the console. Until devices learn membership
  another way, a published policy fills each group's `members` from the
  directory, because the device's policy engine reads them.
- **A policy draft is a `PolicyDocument`**, edited one control at a time
  (`edits.rs`: a tier per action class, allowed agents, the most
  sensitive label, an agent's network destinations, apps in profiles) and
  validated whole after each edit. *Preview impact* compares the draft
  with what the broad ring runs: users in changed groups (everyone, for
  org-wide changes), the agents those people may run, agents whose own
  rules changed, the changes in words and `Policy::lint`'s warnings.
- **Versions and rings (AD-3, AD-11 for policy).** *Publish* signs the
  draft as the next version and releases it to the pilot ring; *Promote*
  moves it to early, then broad. A device gets the newest version its
  ring has reached; IT sets a device's ring on the Devices page (new
  devices join broad). *Roll back* signs an earlier version's content as a
  new version, everywhere at once: versions only go up.
- **Catalog (AD-5, DV-10, DV-11).** Items carry versions, declared
  privileges (DV-3), the submission's reported checks and a review state;
  IT approves for a group as required, available or blocked, or blocks
  the item. Every decision signs a new catalog release (approved items
  only) for every device at once; item rings and pinning enforcement are
  DV-12 (P1). Item ids are agent ids, unique across kinds, since apps and
  agents share one namespace on devices.
- **The store rule (DV-13).** `CatalogItem::public_store_check` refuses
  anything the org owns (every item in its catalog, bought or built) and
  anything built on a managed device; only a single person's app built on
  an unmanaged device passes. Unknown provenance counts as the org's. The
  console's "publish to the public store" answers with the rule.
- **Fleet (AD-4, AD-10 stub).** A device enrolls by syncing with the org's
  enrollment token; the fleet view aggregates its reports: running agents,
  people waiting, policy blocks in 24 hours, versions in force, errors.
  Zero-touch enrollment and per-device credentials are not built.

### Signing (DV-1, DV-10, EP-12)
- The org's **ed25519 key** is generated by the console on first start
  (32 bytes from the system's random source), kept in its state folder at
  mode 0600 and never sent anywhere (`ed25519-compact`: MIT, no
  dependencies). Hardware keys and key ceremonies (EP-11) replace the file.
- A release is an envelope that signs the document **kind, org, version,
  issue time and payload SHA-256**. A catalog can't pass for a policy, one
  org's release can't be replayed at another's devices, and a device
  refuses any version not above the one it has.
- Devices trust the **public keys in their own `glassd.toml`**
  (`[managed] org_keys`, written at enrollment), never a key a response
  names. Several may be listed, so the org key can rotate (EP-12).

### Devices (ADR 0021's proposals, now built)
- **Boot is unchanged**: glassd starts from the policy file on disk, the
  last good one if it doesn't validate. Once it is up, a thread puts back
  the catalog last applied (`<state_dir>/managed/`, checked again), and if
  the policy file isn't the signed release in force (edited by hand, or
  rejected), puts the release's text back.
- **Then it long-polls** (`POST /device/v1/sync`, held up to 25 s until
  something newer is released), so an online device has a release about a
  tenth of a second after IT publishes it to its ring. Each release is
  verified, then validated whole: the policy parsed and linted against an
  action registry built from it, the catalog checked for one item per id.
  The policy is written with `PolicyStore::apply` (the previous kept) and
  swapped into the runtime with its registry in one step
  (`Runtime::apply_policy`). Tasks and app sessions started after use it;
  running ones keep the effective policy they started with (they do use
  the new registry, which differs only in the IT agent's fix list). The
  model gateway is built at start only: a release that changes model
  providers takes effect at the next restart. Every apply and refusal is
  audited (`PolicyLoaded` with `source: managed`, `CatalogLoaded`,
  `ManagedRefused`).
- **Offline**, a failed sync backs off (2 s doubling to 5 minutes) and
  changes nothing: the device enforces what it last applied and
  `glassctl status` says since when the console has been out of reach.
- **Transport** is `ConsoleClient` in `glass-gateway-providers`, still the
  only crate that leaves the machine: HTTPS through the environment's
  proxy, or plain HTTP to a loopback address only with `[managed]
  allow_insecure_http`, which glassd accepts only with the development
  backend. The signature, not the channel, is what devices trust. The
  enrollment token comes from a file only glassd's user can read and never
  appears in a log, record or error.
- The device **reports** ids, login names and counts only (running agents,
  open asks, blocks in 24 hours, versions in force, the last error), never
  task text or paths, and a Glasshouse version, never the base system's
  (BR-1).

### Profiles by role (AD-7)
- **Which apps a person gets is policy**: `[[group]] apps` puts apps in
  members' profiles, managed from the console like everything else.
  Everyone gets Mail and Calendar; IT's tools for managing a computer (the
  admin console, Activity Monitor, Terminal) are in `it-admins`' profiles
  only.
- **glassd enforces it**: `apps.list` (`glassctl apps`) returns the
  caller's profile, which is all a launcher may offer; `app.open` refuses
  an app outside it, audited as `AppRefused`. An id in a profile can't be
  an agent's (one namespace).
- **The on-device admin UI is the console itself**: `console` in a
  profile is listed with the console's address from `[managed] url`, and
  it reads from the console's servers, not from the device. Only a managed
  device has one to list.

### Catalog first (GA-2)
`catalog.search` (`crates/apps/catalog`) is a read-only action at Auto:
given a need in a sentence, it returns approved items the user may
install, best match first, each with how it reaches them and its declared
privileges. It searches the device's synced catalog only, and scopes
results by the user's groups as the runtime resolved them from policy when
the task started (`TaskContext::groups`), never by anything in the
request: an item none of the user's groups gets, or one any of them
blocks, is never returned. It is audited like every action.

## Changed since the first draft of this ADR
The product owner's direction (AD-6, AD-7, DV-13) changed the first
slice's plan:
- The console was to be its own Cargo workspace (`console/`) on axum, with
  its own web build. It is a binary in the main workspace on the bridge's
  small HTTP server (`glass-http`), and its web app is in `apps/` on the
  shared SDK, build and Playwright setup.
- `CatalogItem::publicly_listable` (refused only items signed with the
  org's key) became the store rule, which refuses every org item and
  anything built on a managed device.
- Profiles by role were added (`[[group]] apps`, `apps.list`, `AppRefused`),
  with the admin console as a profile entry.
- Catalog ids became agent ids, unique across kinds; a sync became a POST
  so devices reuse the one HTTP client.

## Consequences
- The device contract lives in `glass-managed`; changing it is changing a
  wire format between our servers and every device. Add fields with
  `#[serde(default)]`.
- Group membership travels inside each policy release until SCIM-backed
  device identity exists: every device gets the whole org's membership.
  Fine for pilots; at fleet scale membership should come per device.
- The catalog says who may *install* an item; policy still says who may
  *run* an agent (`[[group]] agents`). Approving an agent for a group
  doesn't yet allow it in policy; the console shows both. Installing
  catalog items on devices isn't built: `required` is recorded and synced,
  not yet pushed.
- Declared privileges are shown and searched, but the runtime doesn't yet
  intersect an agent's with policy (ADR 0008's `effective = declaration ∩
  policy ∩ scope`); apps' manifests are (ADR 0024).
- The shell's launcher doesn't read `apps.list` yet, and native apps
  (Activity Monitor, Terminal) are started by the shell, not glassd: until
  the launcher filters by the profile and those apps check it, glassd's
  enforcement covers web apps only. Devices have no browser yet to open
  the console entry.
- Blocking an item removes it from the next catalog within a second on
  online devices, but revoking a signature fleet-wide and disabling copies
  already installed (DV-5) is not built.
- An offline device enforces its last policy indefinitely; whether it
  should expire (ADR 0021) is still open.
