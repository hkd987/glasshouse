# 0033. The shared space: artifacts, docs and sheets are sealed conversations

Status: **Accepted** (2026-10-20). PRD AR-1 to AR-6, DC-1 to DC-6, SH-1 to
SH-3, GA-6, and the owner's decision of 2026-10-20 that changes NV-3.
Builds on ADR 0030 (chat), 0024 (the app bridge), 0026 (apps) and 0032
(remix and share).

## Context
- People and their agents make artifacts all day: HTML pages, charts,
  small tools, reports. Today they travel as files dropped into chat, are
  downloaded and opened in a browser, and lose every comment. The owner
  calls sharing them, with feedback, a key system of the OS.
- The owner also asked for Docs and Sheets with AI built in, shared with
  colleagues (reversing NV-3), and GA-6 asks where apps people make keep
  their data.
- Chat (ADR 0030) already solves the hard parts for content between
  people:
  - each person has a member key per computer;
  - a conversation has epoch keys sealed to its members' keys and to
    IT's compliance key, with a new epoch when someone leaves;
  - messages are sealed and signed;
  - the console stores and relays them without reading them;
  - glassd syncs, opens and audits them, and retention and holds apply.
- A sealed chat message is 16 KB at most. An artifact can be megabytes.

## Decision
1. **Everything people make together is a conversation.** New conversation
   kinds sit beside direct, group, channel and meeting:
   - `artifact` (now);
   - `doc` and `sheet` (next);
   - `app_data`, for apps people make (GA-6, which closes ADR 0032's R4).

   Its members are its audience. Its sealed name is its title. Its
   messages carry what happens to it, in a structured `part` inside the
   sealed body: a version, a comment, a reaction, a resolve, an edit.
   Membership, epochs, the compliance key, sync, retention, holds and
   audit are chat's, unchanged. The console stores what it stores for
   chat, and reads none of it.
2. **Large content goes in sealed blobs.** A version's bytes are sealed
   with a fresh random key (AES-256-GCM). The blob goes to the console's
   blob store, and the key travels only inside the sealed message that
   names the version, with the blob's SHA-256. The console stores the
   ciphertext under a random id and the conversation it was uploaded
   for. It hands a blob only to a computer registered to a member of that
   conversation. A blob is 1.5 MB at most: it travels as hex inside the
   console's 4 MB request limit. A larger page waits for chunked blobs.
3. **Clear metadata is what IT needs, and no more.** The console keeps in
   the clear:
   - the kind, its members, who shared it and when;
   - its label;
   - sizes and the number of versions and comments.

   That is AR-6's inventory. Titles are sealed, and IT reads them with its
   compliance key in its own browser (ADR 0030 §4). It follows that
   nothing on our servers reads content:
   - search is the computer's index (NV-30);
   - AI runs on the computer, through the model gateway;
   - export happens on the computer;
   - a view-only link outside the org (AR-5, P1) carries its key in the
     URL fragment and is opened in the outsider's browser, never on the
     server.
4. **Artifacts are shown, never downloaded.** The Artifacts app lists what
   was shared with the person, shows one with its comments beside it, and
   lists their own.
   - The artifact itself is served by the person's app bridge on an
     origin of its own, `<hex>.artifact.localhost` (its conversation id's
     digits; `/` is the latest version, `/v<n>` the nth). The bridge reads
     it with `artifact.read` through the Artifacts app's session, so the
     read is decided and audited like any. No artifact's host names an
     app, so its page can't reach a bridge.
   - Its CSP starts with `sandbox allow-scripts` (no `allow-same-origin`),
     so it has an origin that is no one's even when opened outside the
     app: no cookies, no storage. It allows its own inline scripts and
     styles and data: and blob: images, fonts and media, and nothing else:
     no network, no frames, no workers, no forms, no base URL. Only the
     Artifacts app may frame it (`frame-ancestors`), and its referrer is
     never sent.
   - So a page that loads a library from a CDN doesn't run it; agents are
     told to inline what a page needs. A later slice may serve a fixed
     shelf of common libraries from the bridge.
   - The app shows it in an iframe sandboxed with `allow-scripts` only, so
     it reaches neither the app nor the person's data.
   - The bridge adds a small script to the page for pinned comments. It
     reports where a person clicked as an element path, and where pinned
     elements are, by postMessage. The app sends it only paths, never
     comments, and draws the pins itself. A hostile artifact can only
     misplace pins on itself.
   - Two ways out aren't CSP's to govern:
     - The page navigating itself to another site, carrying what it
       holds (its own content). The viewer counts the frame's loads and
       puts the artifact back, with a notice, when it navigates.
     - WebRTC. Our engine build (CF-15) must refuse both to artifact
       origins. Until then WebRTC is the same gap every app has (ADR 0030
       §6).
5. **Feedback is messages.** A comment is a message with an optional
   anchor (an element path or a point, and the version it was made on).
   Reactions and resolves are messages too. A new version's comments
   carry the version they were made on, so the app shows older ones as
   about an earlier version (AR-4).
6. **Actions** (crate `glass-app-artifacts`; it reaches conversations
   through a `Shared` trait in `glass-managed` that chat's store
   implements, as Calendar reaches meetings, so no app depends on Chat):

   | Action | Tier | Notes |
   |---|---|---|
   | `artifact.list` | Auto | Shared with the person, and their own |
   | `artifact.read` | Auto | A version's HTML and its comments |
   | `artifact.share` | Ask | A new artifact to people, groups or a conversation. Irreversible: others see it. |
   | `artifact.update` | Ask | A new version, with a note |
   | `artifact.comment` | Ask | |
   | `artifact.resolve` | Ask | Others see it too |

   The Artifacts app's own tiers (`[[app]]`, as Chat's) make the person's
   own share, comment and resolve Auto: what they typed in the app is
   theirs. Agents ask.
7. **In Chat an artifact is a card.** Sharing to a conversation also posts
   a message there whose part names the artifact. Chat shows it as a card
   that opens the Artifacts app, never as an attachment.
8. **Docs and sheets** (next slices) are conversations whose messages are
   CRDT updates (Yjs, MIT), sealed like any message. Snapshots go in
   sealed blobs.
   - The console relays updates it can't read and never merges them.
   - The pages merge.
   - A computer posts a snapshot when the log grows. The first one to ask
     does it, as with a meeting's new key.

   Sheets' formulas are ours to write: the common open formula engines
   are GPL, which `deny.toml` refuses.

## Consequences
- One sealing and sharing model for everything people make. Retention,
  holds and audit cover artifacts', docs' and sheets' messages with no new
  code on the console's side.
- The console's blob store is the one new thing on our servers. It holds
  ciphertext and ids only, and refuses an id it holds already, so no
  member can replace another's version. Retention and holds don't reach
  blobs yet, there is no quota per conversation, and a blob whose
  message never arrived isn't cleaned up: all three come before a pilot
  org (A2).
- The Chat app shows only chat's kinds. Artifacts, docs, sheets and
  meetings each have their app.
- An artifact's audience changing means a new epoch, as in chat. Someone
  removed keeps what they already saw.

## Slices
- **A1:**
  - the kinds, parts and blobs in `glass-managed`;
  - the console's blob store;
  - chat's store opening items;
  - `glass-app-artifacts`;
  - the bridge's artifact origins;
  - the Artifacts app with comments;
  - Chat's card;
  - end to end: an agent shares an HTML page, a colleague opens it and
    pins a comment, and the agent reads the feedback and shares version
    2.
- **A2:**
  - links outside the org (P1, the owner's call on public pages);
  - IT's inventory page (AR-6);
  - retention, holds and quotas for blobs, and cleaning up orphans.
- **D1:** Docs. **S1:** Sheets.

## Amended by ADR 0052
Updates of one artifact take turns on each computer, so two at once make
two versions (`specs/tla/ArtifactsShare.tla`).
