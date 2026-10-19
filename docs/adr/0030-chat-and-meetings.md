# 0030. Chat and meetings: sealed between the org's computers, opened by glassd, readable by IT's compliance key

Status: **Accepted** (2026-10-19): the owner decided chat and meetings are
end to end encrypted, with a compliance key IT holds, meetings org only up
to 25; the design below carries it out. Two choices in it are proposed
defaults the owner can still change (end of this ADR). Slice C1 is built,
on the development host and the VM: `glass-crypto`, the console's relay,
on computers the member keys, store, sync and the four chat actions with
`[[app]]` tiers, `app.status` waits, and the Chat app. Not for a pilot
before P (device keys) and C2 (presence, several computers, membership). Policy with `[[app]]` needs a glassd that knows it
(`PolicyDocument` refuses unknown fields), as `settings` did. PRD CF-91 (our own chat), CF-21 (Glasshouse meetings), CF-61 … CF-65, GT-3, AU-1, AU-10, NV-1 … NV-3, CF-41, UI-2, UI-5, UI-7, DV-3. Builds on:
- ADR 0018: the base system's OpenSSL.
- ADR 0023: the console, signed releases, the device API.
- ADR 0024: apps are principals.
- ADR 0027: the engine.
- ADR 0028: per-device keys, the desktop's pid.
- ADR 0029: camera and microphone settings.

## Context
The owner decided these on 2026-10-19:
- **Our own chat.** Glasshouse ships its own chat, like Teams or Slack: channels and direct messages, for people in the org.
  - Messages are end-to-end encrypted between the org's computers. Our servers keep ciphertext and metadata only (GT-3).
  - IT holds a compliance key it keeps itself, never on our servers. That is how retention, legal hold and eDiscovery (CF-61 … CF-65) keep working.
- **Agents and chat.** Agents read chat without asking (Auto). They post only with the person's yes (Ask). This goes through actions, like Mail's and Calendar's.
- **Glasshouse meetings.** Video for up to 25 people in the org.
  - Scheduled from Calendar like any invitation, and joined from the event.
  - Camera, microphone and screen sharing.
  - End-to-end encrypted, and never recorded.
  - No guests from outside the org in V1.
  - Apps run in the device's engine, so the page has WebRTC and WebCrypto.

What is already there, and what it forces:
- **glassd's state.** glassd runs as root. It keeps its state in `/var/db/glasshouse` (mode 0700).
- **Enrollment.** Devices enroll with one token per org. They long-poll the console for signed releases (ADR 0023).
- **No per-device credentials.** The console knows a device only by the name it gives. ADR 0028 names the same gap.
- **The signing key.** The console holds the org's signing key, on our servers. Devices believe whatever it signs.
- **Signing in.** People sign in to a computer with a local account (ADR 0019). Signing in through the org's identity provider is CF-1, and it is not built.
- **Apps have no network.** An app reaches the system only through actions on glassd's app socket. `AppManifest::check` refuses `network` (ADR 0024).
- **Offline reads land in the person's home.** The SDK's `bridge.read()` keeps every successful read in the page's storage. That storage is the engine profile, under the person's home (ADR 0027).
- **Tiers can't be lowered for an app.**
  - Between groups and an agent's rules, the strictest tier wins.
  - `Policy::for_app` gives an app session only the org's and the groups' tiers.
  - So with `"chat.post" = "ask"`, a person typing in Chat would approve every message on the desktop. Mail already asks for every send today (ADR 0024, "Not done").
- **Every action is audited.** A page that polled `chat.read` every two seconds would flood the log.
- **Labels come from paths.** Chat isn't a file.
- **Invitations stay on the computer.** Calendar invitations don't leave it yet; they are written to `~/Calendar/Sent`.
- **OpenSSL is already in the tree.** `openssl` 0.10.81 (Apache-2.0) and `openssl-sys` 0.9.117 (MIT) are in `Cargo.lock`, through native-tls.
  - They expose AES-GCM, HKDF, ECDH and ECDSA on P-256, and the system's random source. Each was checked in the registry's copy.
  - They have no HPKE.

## Decision

### 1. Keys: one per person per computer, one per conversation epoch, one for IT

```text
compliance key   IT's, P-256. The private half exists only in a file IT keeps.
   ▲ sealed to it: every epoch key of every conversation (never a meeting's media key)
member key       one per person per computer: a P-256 pair for agreement and one for
                 signing, in glassd's state folder, root only
   ▲ sealed to it: the current epoch key of each conversation the person is in
epoch key        32 random bytes per conversation; a new one whenever someone leaves
   ├─ seals the one before it, so the newest key opens the whole history
   └─ derives one key per message (HKDF); messages are AES-256-GCM
device key       one per computer, P-256: signs the computer's requests to the console
```

- **One cipher suite.** P-256, SHA-256, HKDF-SHA256 and AES-256-GCM.
  - Sealing a key to a public key is HPKE base mode (RFC 9180). The suite is DHKEM(P-256, HKDF-SHA256), HKDF-SHA256, AES-256-GCM.
  - It is checked against the CFRG's published test vectors. Their file has a base-mode vector for exactly this suite.
- **Why P-256 rather than X25519.**
  - IT makes and uses the compliance key in whatever browser it runs. Every browser's WebCrypto has P-256.
  - OpenSSL on the computer can run P-256 under a FIPS-validated provider. Banks' reviewers ask about that.
- **`glass-crypto` (`crates/lib/crypto`) is a new lib crate.** It is the only place this code lives.
  - It covers seal and open, sign and verify, and the message and epoch formats.
  - It is built on the `openssl` crate, which uses the base system's OpenSSL (ADR 0018).
  - The workspace names `openssl` as a direct dependency. The layering check allows it in `glass-crypto` and `glass-gateway-providers` only.
  - The same formats are written once more in TypeScript, on WebCrypto (`apps/sdk/src/seal.ts`). That copy serves IT's browser and meeting frames. Both copies run the same vector files.
- **Member keys are per person per computer.**
  - Why not one key per person: it would have to be copied between computers, or kept by us.
  - Per computer, nothing secret leaves a computer except sealed to another member key. A lost laptop is cut off on its own.
  - glassd makes the pair the first time the person opens Chat on that computer. An agent's call never makes it: until then an agent's `chat.list` says the person hasn't opened Chat, and its posts are refused. It lives in `/var/db/glasshouse/chat/<login>/member.key`, mode 0600.
  - glassd registers the public halves with the console, signed by the device key.
  - A member key's id is `mk-` plus the first 16 hex digits of the SHA-256 of both public halves.
- **Epoch keys are chained backward.**
  - The computer that creates a conversation makes its first epoch key.
  - A new epoch key is made when anyone leaves or is removed, a computer is revoked, or a person leaves the org.
  - Each epoch record carries the previous epoch key, sealed under the new one.
  - So whoever holds the newest key can open every earlier one. Handing over history means handing over one key.
- **Each message has a clear header and a sealed body.** The header is metadata, and it is the AEAD's associated data.

  ```text
  header  {"v":1, "org":"acme", "conversation":"cv-0192…", "epoch":4,
           "id":"0192f3…" (UUIDv7, chosen by the sender), "sender":"priya", "key":"mk-3f9a…"}
  body    {"text":"…", "attribution":"by claude-code for priya" | null,
           "reply_to":null, "sent_at":"2026-10-19T14:02:11Z"}
  key     HKDF-SHA256(epoch key, info = "glass-chat msg v1" ‖ conversation ‖ epoch ‖ id)
  seal    AES-256-GCM, random 96-bit nonce, AAD = header
  sign    ECDSA P-256 by the sender's member key, over SHA-256(header ‖ nonce ‖ ciphertext)
  ```

- **The signature says who wrote it.** Without it, the epoch key would prove only "someone in the conversation".
  - The console checks it, so nobody can post as someone else.
  - Every reader checks it too, so the console can't either.
- **Names and topics are sealed.** A conversation's name and topic are sealed under its current epoch key, and sealed again at each new epoch. The console sees an id, a kind and the members.
- **Members sign membership changes.**
  - Creating, adding, removing and leaving are ops. The member who makes an op signs it, and each op carries the hash of the one before.
  - A computer walks this chain before it seals a key to anyone. So the console can't add a member on its own.
  - IT can remove people, for offboarding. Those ops are signed by the org key, which devices already trust.
  - IT can't add anyone to a conversation.
  - A person dropped from the directory release (ADR 0023) is removed from every conversation.
- **People who are added see the conversation's history, as in Slack.**
  - Whoever adds them seals the current epoch key to their member keys.
  - If the owner prefers Teams' choice, where added people see no history, the alternative is to make a new epoch on every add.
- **New and replacement computers get history too (CF-41).**
  - The console keeps a list of gaps: member keys that lack the current epoch key. The list is metadata only.
  - It offers each gap to one online member computer on that computer's next poll, with a 60-second lease.
  - That computer checks the chain and fills the gap. Every key it gives away is audited on that computer.
  - If no other member of a conversation ever comes online again, the new computer never gets that history. IT's export still has it.
- **Rotation without forks.**
  - A new epoch is posted as "based on epoch N". The console takes the first one and refuses the rest (compare and swap).
  - A computer that loses fetches the winning epoch.
  - The console refuses a message for an epoch older than the conversation's current one. So a removed person's computer can't post into the conversation.
  - When an honest post hits a new epoch, glassd seals it again and retries. The person doesn't see this.
- **Limits in V1.**
  - Up to 1,000 people in a conversation. A rotation is then about 1,500 envelopes of about 180 bytes each.
  - A message is at most 8,000 characters, and 16 KiB once sealed.
  - Files come later.

### 2. glassd opens chat; Chat and agents read through actions
- **Decryption happens in glassd, never in a page.**
  - Keys stay with root.
  - Agents read through actions, so policy, labels and the audit log apply (AU-1).
  - The Chat page sees only the text it shows, so a bug in the page can't leak keys.
  - One implementation serves the app, agents and offline reads.
  - The cost: glassd keeps a store and talks to the console for chat. It already talks to the console for releases.
- **The store is `/var/db/glasshouse/chat/<login>/`.** It holds:
  - the member key;
  - the conversations, with their membership chains;
  - the conversations' epochs, whole: the maker's signature covers every envelope, so a relay can't drop any;
  - messages, as the verified ciphertext the console sent. Each read opens them.
- **Nothing goes in the person's home,** so their own processes reach chat only through actions. That includes their attended harness.
- **The page keeps nothing either.**
  - Chat calls `bridge.call`, never `bridge.read`. It declares no `offline` permission, because the engine profile is in the person's home.
  - Chat still works offline (UI-5). glassd is on the same computer, so "offline" for chat means the console is out of reach. glassd's store covers that.
- **Sync.** A glassd thread long-polls `POST /device/v1/chat/sync`. Each poll is held up to 25 s, ADR 0023's pattern. It does this for every person with a member key on the computer.
  - It checks every record: the signature, the chain, the epoch, the sender's key.
  - It keeps what passes. What fails is dropped and audited as `ChatRefused`.
  - It wakes waiting app sessions.
  - It does the jobs the console offers: fill a gap, or make a new epoch.
  - It goes through `ConsoleClient` in `glass-gateway-providers`, still the only crate that leaves the machine.
  - `glass-app-chat` sees it as a `ChatLink` trait in `glass-managed::chat`, so tests can use a fake.
- **Live updates without flooding the audit log.**
  - `app.status` gains `wait_secs` (up to 25) and `since`.
  - It answers as soon as the person's chat changes, with ids only: `{"changed": ["chat"], "cursor": 412}`.
  - The page then makes one audited `chat.read`.
  - This is a method like `app.status` today, not an action, and it carries no content.
- **Posting needs the console.** If the console can't be reached, a post comes back as a readable refusal. It is never queued (ADR 0024).
- **Unmanaged or not set up.**
  - An unmanaged computer has no chat: "This computer isn't managed by an organization, so there's no chat."
  - A managed org has chat once IT has made the compliance key (decision 4). Until then every chat action says IT hasn't set chat up.

### 3. Actions, tiers, previews, audit
A new app crate, `glass-app-chat` (`crates/apps/chat`), is registered in `glass-first-party`.

| Action | What it does | Side effect | Org tier | In the Chat app |
|---|---|---|---|---|
| `chat.list` | The person's conversations: name, kind, people, unread, last activity | none | auto | auto |
| `chat.read` | One conversation's messages: the newest 50 (up to 200), or before or after a message. Each comes with sender, time, text and by-line | none | auto | auto |
| `chat.search` | Text across the person's conversations, up to 50 hits (slice C4) | none | auto | auto |
| `chat.post` | One to 20 messages, each to a conversation or to people (opening the DM or group) | irreversible | ask | auto, with presence |
| `chat.create` | A named channel with people | irreversible | ask | auto, with presence |
| `chat.members` | Add or remove people, or leave (slice C2) | irreversible | ask | auto, with presence |
| `chat.delete` | Delete one's own message (slice C2) | irreversible: others may have read it | ask | auto, with presence |

- **Inputs** parse into structs with `#[serde(deny_unknown_fields)]`.
  - `chat.post` takes `{"posts": [{"conversation": "cv-…" | "to": ["sam"], "text": "…", "reply_to": "…"?}]}`.
  - Text is 1 to 8,000 characters. Logins must be in the directory.
- **Previews.**
  - Summary: "Post in #northwind-deal", "Message Sam Lee", or "Post 3 messages".
  - Targets: the conversations' names.
  - Items (the review list, DA-12): "#northwind-deal: <the text as one display line, up to 300 characters>". The text goes through `display_line` (PA-3).
  - Ask labels: "Review messages" and "Approve and post".
  - A completed post is a `Sent` change. It is not reversible, and undo lists it as something that left the machine.
- **Audit (AU-1).**
  - Each call is an Action record as today: user, agent, task, action, targets, decision, approval.
  - The summary never carries message text.
  - `detail.chat` holds the conversation id, the message id and the SHA-256 of the ciphertext. That ties the computer's record to the console's copy for eDiscovery (AU-3).
  - New event kinds:
    - `ChatKeyMade`: a member key was made for a person.
    - `ChatKeyShared`: this computer gave a conversation's key to a person's computer, naming both.
    - `ChatEpoch`: this computer made a new epoch, and why.
    - `ChatRefused`: a record from the console failed a check and was kept out.
  - If the audit log is down, nothing is posted (fail closed).
- **Refusals are results.** Each of these comes back as a result the agent or the page can read, never as a protocol error:
  - declined on the desktop;
  - a group rule at Never;
  - not a member;
  - IT hasn't set chat up;
  - the console is out of reach.
- **Attribution (AU-10).** When an agent posts, glassd puts `by claude-code for priya` in the sealed body. It takes this from the task's principal, never from the request. Every reader's Chat shows it inline.
- **Default policy.** Each line lands with its action's slice. `claude-code` also gains `chat.list`, `chat.read`, `chat.search` and `chat.post`.

  ```toml
  [org.tiers]
  "chat.list" = "auto"
  "chat.read" = "auto"
  "chat.search" = "auto"
  "chat.post" = "ask"
  "chat.create" = "ask"
  "chat.members" = "ask"
  "chat.delete" = "ask"

  # The person's own messages from Chat don't ask: they just typed them.
  [[app]]
  id = "chat"
  tiers = { "chat.post" = "auto", "chat.create" = "auto", "chat.members" = "auto", "chat.delete" = "auto" }
  ```

- **The person's own messages run at an app tier that presence holds.**
  - Policy gains `[[app]]`: tiers for one app's sessions, and nothing else. The manifest still says what the app may call.
  - `Policy::for_app` uses those tiers as the session's own rules, so a stricter group still wins. A group that sets `"chat.post" = "ask"` asks its members for every message.
  - A tier that an `[[app]]` rule lowers below the org's holds only for a call the person just made in that app's window.
    - The desktop tells glassd when a real key press or click lands in an app's window. The desktop is registered by pid, as ADR 0028 step 1 requires.
    - A lowered tier needs such an input in the last 5 seconds. Otherwise the call takes the org's tier, and the person sees an ask.
  - Why presence is needed: without it, any of the person's processes could open a session as Chat through the bridge (ADR 0024, 0027) and post at Auto. An agent that read a message telling it to do exactly that is the attack.
  - Presence comes in slice C2. Nothing reaches a pilot before it.
  - Why not a second action, such as `chat.send` at Auto for apps only: it would be simpler but unsafe, because an agent given `chat.*` would get it.
  - The same rule lets Mail send what the person typed without asking, later.
- **Labels.**
  - Chat reads should carry the `internal` label. `Preview` gains a label for data that isn't a file. The broker checks it against the agent's limit and adds it to the task's label, as it does for a path (slice C4).
  - Until then an agent limited to `public` can read chat. ADR 0023 says the same of the directory.
- **What the Chat app itself calls.**
  - On open: `chat.list`.
  - For the open conversation: `chat.read`.
  - On Enter: `chat.post`.
  - New message: `people.search`, then `chat.post` with `to`.
  - New channel: `chat.create`.
  - For live updates: `app.status` with `wait_secs`, in a loop.
  - Everything goes through `bridge.call`.
- **Chat's manifest** is `apps/chat/manifest.json`.
  - Actions: `["chat.list", "chat.read", "chat.search", "chat.post", "chat.create", "chat.members", "chat.delete", "people.search"]`.
  - `permissions: []`, `labels: ["public", "internal"]`, and no `network`.
  - Everyone's profile gets Chat (`[[group]] everyone apps`).

### 4. Compliance: IT's key, retention, holds, export
- **IT makes the key in its own browser,** on the console's new Chat page.
  - WebCrypto generates a P-256 pair.
  - The private half is exported (PKCS#8) and wrapped with a passphrase IT types (PBKDF2-SHA256, 600,000 rounds, AES-256-GCM). It is saved as a file on IT's computer.
  - The page tells IT to keep two copies, stored apart.
- **The key is proven before it goes live.**
  - The console seals a random challenge to the public half. The page opens it from the saved file.
  - So a file or passphrase that doesn't work is caught now, not at the first subpoena.
- **Only then does the key go live.** The console takes the public half and signs it into a new release kind, `DocKind::Chat`.
  - The release carries the compliance key, its fingerprint and retention.
  - It has no rings, so every computer uses the same key at once, as with the directory.
- **The private half never leaves the browser.** The page imports it as non-extractable, and forgets it when the page closes.
- **Every epoch key is sealed to the compliance key.**
  - The console refuses an epoch that lacks an envelope for the current compliance key.
  - The console can't check that the envelope opens. The Chat page's Check does that, in IT's browser, on a sample of recent epochs.
  - Every computer's Chat shows the fingerprint under About, for example: "IT can read this org's chat for compliance. Key 4F2A 9C1E …". IT can compare it with its file on any computer.
- **Rotating the compliance key.** New epochs seal to the new key. IT either keeps the old files for old messages, or seals the old envelopes again to the new key, in the browser.
- **Losing every copy of the file loses IT's access** to everything sealed to it. We can't recover it, by design.
- **Retention (CF-61).**
  - Set on the Chat page: days for channels, and days for DMs.
  - There is no default. Chat is kept until IT sets a period.
  - It rides the Chat release.
  - The console deletes ciphertext older than the period unless it is held.
  - Computers delete their copies by the same rule, offline too.
  - Envelopes and chain records are deleted once no remaining message needs them.
- **Legal hold (CF-64).**
  - Set on the Chat page: people, conversations, a date range and a matter name.
  - Holds live on the console only, because that's where the ciphertext is.
  - Held messages, and the envelopes that open them, outlive retention and their sender's delete.
  - A held message its sender deleted is hidden from members, and kept until the hold is released.
- **eDiscovery (CF-65).**
  - IT picks a hold or a query.
  - The console builds a bundle:
    - the ciphertexts and their headers;
    - the compliance envelopes;
    - the epoch and membership chains;
    - the member public keys and the signatures;
    - a manifest of every item's SHA-256, signed by the org key.
  - The browser opens IT's file with the passphrase, opens each epoch key, checks each signature and decrypts.
  - It saves the export on IT's computer: JSON, and RSMF (the Relativity Short Message Format that review tools import).
  - The export includes a custody record: who, when, the query, the bundle's hash, the counts, and any signature that failed.
  - Nothing decrypted goes to our servers.
- **Custody log on the console.** Holds, releases, exports and key changes go in a hash-chained log, in glass-audit's format. The Chat page shows it.
- **Who may use the Chat page.** Members of the groups IT names on it, `it-admins` by default. This is roles only, as ADR 0028 decided for support.
- **Limit: the page's code comes from our servers.**
  - A compromised console could serve a page that leaks the key the next time IT opens it.
  - The compliance page is its own bundle with a published hash, and it is used rarely.
  - The stronger form is the same page as a device app from the signed image (EP-2). That comes later.

### 5. The console
**Device credentials come first.** They are shared with ADR 0028's sessions.
- `POST /device/v1/register` takes the enrollment token and the computer's public key, once.
- After that, every device request is signed.
  - The request carries the device id and the time.
  - The signature covers the method, the path, the time and the body's hash.
  - The time must be within two minutes, and a replay cache stops reuse.
- IT can revoke a computer on the Devices page.

| Route | What it does |
|---|---|
| `POST /device/v1/register` | Registers a computer's device key (enrollment token) |
| `POST /device/v1/chat/keys` | Registers a member key for a login on this computer |
| `POST /device/v1/chat/keys/lookup` | Member keys of logins, revoked ones marked |
| `POST /device/v1/chat/sync` | Long poll: new messages, the conversations' epochs (whole, as signed), ops, tombstones, key changes, jobs |
| `POST /device/v1/chat/history` | Older messages of a conversation, 200 at most |
| `POST /device/v1/chat/conversations` | Create a conversation: kind, sealed name, the creator's signed op, and epoch 1 with every envelope |
| `POST /device/v1/chat/conversations/<id>/ops` | A signed add, remove or leave |
| `POST /device/v1/chat/conversations/<id>/epochs` | A new epoch, based on the current one (compare and swap) |
| `POST /device/v1/chat/conversations/<id>/envelopes` | Fills gaps: the current key sealed to member keys that lack it |
| `POST /device/v1/chat/conversations/<id>/messages` | A post: header, nonce, ciphertext, signature |
| `POST /device/v1/chat/conversations/<id>/messages/<mid>/delete` | The sender's signed delete |
| `POST /device/v1/meet/<id>/join`, `…/leave` | A participant's offer in, the relay's answer back (decision 6) |
| `GET /api/chat` | The Chat page: set up or not, fingerprint, retention, holds, exports, custody log, member keys by person |
| `POST /api/chat/compliance/challenge`, `POST /api/chat/compliance` | Prove and install a compliance key; sign a Chat release |
| `POST /api/chat/retention` | Set retention; sign a Chat release |
| `POST /api/chat/holds`, `POST /api/chat/holds/<id>/release` | Legal holds |
| `POST /api/chat/exports`, `GET /api/chat/exports/<id>` | Build and fetch an export bundle |
| `POST /api/chat/keys/<id>/revoke` | Cut off a member key, for a lost computer |

- **What the console checks, without reading content:**
  - the device's signature;
  - that the member key is registered to the sender, on this computer, and not revoked;
  - that the sender is a member;
  - that the epoch is current;
  - the message signature;
  - sizes, and that ids are unique;
  - that a new epoch's envelopes cover exactly the members' live keys, plus one for the current compliance key;
  - that ops are signed by a member, or by the org key for removals, and chained.
- **After those checks,** it numbers the message within the conversation and stamps the time. Then it wakes waiting computers through a second `watch` channel, beside the one for releases.
- **Data model.** JSON files under the state folder in `--dev`; tables in production.
  - `devices`
  - `member_keys`: id, login, device, public halves, registered, revoked.
  - `conversations`: id, kind, created by and when, members, current epoch, sealed name.
  - `ops`
  - `epochs`: number, based on, reason, made by, sealed previous key, compliance envelope.
  - `envelopes`: epoch, member key, sealed key.
  - `messages`: conversation, number, id, epoch, sender, key, time, size, nonce, ciphertext, signature, deleted.
  - `holds`, `exports`, `chat_releases`.
  - `jobs`: a gap or a rotation, and who it is leased to.
  - the `custody` log.
  - For meetings, `meetings`: conversation, session, relay, participants, and media envelopes until the session ends.
- **The console never holds** plaintext, names or topics, epoch keys, private keys, or media.
- **It does hold the following, and GT-3's wording must say so:** who is in which conversation, who posted when and how much, DM pairs, computers and their keys, holds and exports, and meeting attendance.

### 6. Meetings
- **The media path.**
  - Each participant's engine keeps one WebRTC connection to `glass-sfu`, our selective forwarding relay.
  - The connection is DTLS-SRTP from hop to hop. Inside it, every encoded frame is sealed end to end in SFrame's format (RFC 9605, AES-GCM).
  - The sealing happens in an encoded transform, in a worker in the page: `RTCRtpScriptTransform`, or the engine's older `createEncodedStreams`.
  - The codec bytes the packetizer needs stay in the clear. For VP8, that is the first 10 bytes of a key frame and the first 3 of other frames. For Opus, nothing.
  - The relay forwards packets in str0m's RTP mode, by SSRC and simulcast layer.
  - It reads RTP headers, RTCP, and the audio-level header extension, to know who is speaking.
  - It can't decode a frame.
- **Keys.**
  - A meeting is a conversation of kind `meeting`: the organizer and the invitees, all in the directory. Its epoch keys seal its title and time, like any conversation's name.
  - Media uses a separate, random **media key** for each meeting session.
    - It is sealed to the participants' member keys only, never to the compliance key.
    - The console deletes those envelopes when the session ends.
  - Each sender's frame key is derived from the media key (HKDF), with the participant's index as SFrame's key id.
  - When someone leaves, a remaining participant's computer makes a new media key, through the console's job lease as for chat. The old key is kept for two seconds, for frames in flight.
  - All participants hold the media key, so any of them could forge another's frames. This is SFrame without signatures.
- **Joining.**
  1. Calendar's Join opens the Meet app on the meeting.
  2. The page makes its WebRTC offer and calls `meet.join {meeting, offer}`.
  3. glassd checks that the person is a member, gets or makes the media key, and sends the offer to the console, signed by the member key.
  4. The console checks membership and the 25-person limit, picks a relay, and hands it the offer over its private API.
  5. The answer comes back the same way.
  6. glassd returns the answer, the media key and the participant's index.
  - Everything after that goes over a data channel to the relay: people coming and going, renegotiation, layer choice, who is speaking.
  - The page makes no HTTP request of its own.
- **Media keys stay in the page's memory.** `meet.join` and `meet.keys` are calls (`bridge.call`), and the SDK never caches them. The page holds the media key in memory only.
- **Meet's manifest.**
  - It declares `meet.list`, `meet.join`, `meet.keys` and `meet.leave`.
  - It declares a new permission, `media`. Chat never gets it.
  - ADR 0024's no-network rule rests on the page's CSP, and CSP doesn't govern WebRTC. So `media` is checked in the manifest, and our engine build (CF-15) must allow WebRTC only to origins that declare it.
  - Until our build exists, that is a gap for every app, Meet or not.
- **Agents can't join.** Policy validation refuses any agent rule that names `meet.*`, in glassd and in the console. So an agent's call is Never, and comes back as a readable refusal.
- **ICE.**
  - The relay is ICE-lite on public addresses, with one UDP port for everyone.
  - Computers connect out to it, so there's no TURN in V1.
  - str0m also does ICE over TCP. A passive TCP candidate on port 443 is the fallback for networks that block UDP (slice M3).
- **Never recorded.**
  - The relay has no code that writes media. It runs from a read-only image with no data volume.
  - Media keys are never sealed to anything that outlives the session.
  - Who joined, when and for how long is metadata. It is on the console and in each computer's audit log (`meet.join`, `meet.leave`).
- **Camera and microphone** follow ADR 0029's settings (`browser.camera`, `browser.microphone`). The engine asks for each app origin.
- **Screen sharing** needs the desktop to hand its screen to the engine behind a consent prompt (UI-2). CF-20 needs the same work for Teams and Zoom. Meetings get screen sharing when that lands.
- **Calendar.**
  - `Event` gains `#[serde(default)] meeting: Option<{id}>`.
  - `calendar.create` and `calendar.update` take `"meeting": true`.
  - `calendar.invite` on a meeting refuses anyone not in the directory, as a readable refusal: "Glasshouse meetings are for people in the org for now; x@other.com isn't in the directory". It also refuses more than 24 invitees.
  - Once the invite is approved, it also creates the meeting's conversation with the invitees, signed by the organizer's member key.
  - Invitations don't reach other people's calendars yet. So invitees' Calendar shows the meetings they're in through `meet.list`, which Calendar declares, with Join.
- **The relay library is str0m 0.24.1** (MIT OR Apache-2.0), with `default-features = false, features = ["openssl"]`.
  - So the relay uses OpenSSL, like everything else.
  - str0m is a WebRTC library, not an SFU. These parts are ours to write: rooms, forwarding, layer choice, the 25-seat limit, and the console's private API.
  - They live in `crates/svc/media-relay` (`glass-media-relay`), run by `crates/bin/glass-sfu`, which runs on our servers and is never in the image.
  - str0m's own README says a production SFU is significant work.
- **The other libraries considered:**
  - webrtc-rs (MIT or Apache-2.0): a full async peer-connection stack, heavier than a relay needs.
  - mediasoup (ISC): a C++ worker with a meson build, which adds a second language.
  - LiveKit and Pion (Apache-2.0, MIT): Go services, outside our workspace and its license checks.
  - Janus: GPL-3.0, refused.
  - Jitsi Videobridge (Apache-2.0): needs a JVM.

### 7. MLS (openmls) later, not first
MLS (RFC 9420) would add:
- **Forward secrecy within a group.** Per-message keys come from a secret tree and are deleted after use. Here, an epoch key stolen from a computer opens everything in that epoch, and through the chain, everything before it.
- **Post-compromise security.** Members update their leaves and the group heals. Here, a stolen computer can read until it's revoked and a new epoch is made.
- **Rekeying in log(n).** A removal touches about log2(n) path secrets. Here a removal seals to every remaining computer, which is fine up to the 1,000-person limit.
- **A published standard with published analysis.** Membership changes are part of the protocol; here they are our own signed chain.
- **SFrame keys from MLS's exporter,** as RFC 9605 suggests.

What MLS would not change, and what it would cost:
- It wouldn't change the trust in whoever vouches for who owns a key. Here, that is our console.
- Our two needs pull against it:
  - IT must be able to open every message. That means exporting every epoch's secret to the compliance key, which undoes much of the forward secrecy.
  - New computers must get history, which MLS deliberately doesn't give.
  - Both would be ours to bolt on.
- IT's browser would need MLS's message format to decrypt an export, which means openmls compiled to WebAssembly.
- openmls itself is MIT. But both of its crypto providers, `openmls_rust_crypto` 0.6.0 and `openmls_libcrux_crypto` 0.4.0, depend on `hpke-rs` 0.8.0. That is MPL-2.0, which `deny.toml` refuses. Using openmls would mean writing our own provider on OpenSSL, HPKE included.

So we start with the scheme above.
- Every conversation names its scheme (`"scheme": "epoch-v1"`). An `mls-1` conversation can come later, beside it.
- Revisit when a customer needs forward secrecy against a stolen computer more than history on new computers, or when there's a permissively licensed provider.

## What this trusts
- **Our console, as the key directory.**
  - It can't read what it stores.
  - But it says which computers a person has, and it holds the org's signing key. A compromised console could register a computer for someone, or swap the compliance key. It could then read new messages, and through gap filling, history.
  - That would show, not hide:
    - the person's other computers, and every conversation, show "Priya added a computer (LT-0423)";
    - every key a computer gives away is in that computer's audit log;
    - every computer shows the compliance key's fingerprint.
  - Preventing it needs sign-in through the org's identity provider (CF-1) to vouch for member keys. After that, key transparency, or an IT-held key that approves new computers.
- **Every enrolled computer's word on who is signed in.**
  - Until CF-1, a computer registers member keys for whatever login it claims.
  - So anyone with the org's enrollment token and root on an enrolled computer could claim to be a colleague.
  - The console accepts only logins that are in the directory, and the notices above show it.
- **glassd on each computer.** Root can read the chat of everyone who uses that computer.
- **No forward secrecy within an epoch.**
  - A stolen computer's state opens everything it held, until the computer is revoked.
  - Disk encryption (AD-12) protects that state at rest. Keys sealed in the TPM come later.
- **Members.** Removing someone protects only later messages. What they read, they keep.
- **The pages.** The Chat page's memory holds only what it shows. The Meet page holds the media key in memory only. Neither writes chat or keys to the engine's storage in the person's home.
- **Metadata,** as listed in decision 5.
- **IT, by design.** The compliance key opens every message. The page that uses it comes from our servers (decision 4).
- **The app socket's word on which app is calling,** until presence lands (decision 3).
- **Meetings.** The relay sees who, when, packet sizes and timing, and who is speaking. Participants share one media key.

## Containment review
- **Reach.**
  - Nothing new in a jail.
  - Agents reach chat only through actions on their task socket.
  - The store and the member keys are in glassd's state folder, which no jail mounts. A new probe check proves it: `chat: read the chat store`.
- **Data leaving.**
  - `chat.post`, `chat.create`, `chat.members` and `chat.delete` leave the machine sealed, and stay at Ask for agents.
  - glassd reaches only the console it already syncs with.
  - The Meet page reaches the relay that the answer names. That is a new destination, declared by `media`.
- **Root acting for an agent.**
  - glassd writes only under its state folder, never in a home.
  - Logins must be in the directory.
  - Conversation and message ids are checked against their patterns before any path is built from them.
- **Decisions.**
  - Every chat call passes the manifest check, `decide()`, labels (once chat has one) and asks.
  - Irreversible calls stay at Ask for agents.
  - An app's lowered tier holds only with presence.
  - `meet.*` is never an agent's.
- **Identity.**
  - By kernel uid on the app socket, as today, with presence for lowered tiers.
  - Device requests are signed by the computer's key.
  - Member keys are bound to logins by the console (see "What this trusts").
- **Record.**
  - Every call is audited, refusals included, without message text.
  - Making keys, sharing keys and refusing records are their own events.
  - An audit failure stops posts.
- **Lifetime.**
  - An app session ends with its connection.
  - Leaving or closing Meet ends the participant's media, and the page forgets the key.
  - Member keys belong to the person, not to a task. They stay until IT revokes them or the person leaves.
- **Undo.**
  - Posts, creates, membership changes and deletes are listed as things that can't be undone.
  - `chat.delete` is not undo: others may have read the message.

## Building it
Every test name carries its PRD id. Nothing reaches a pilot org before slices C2 and C3.

**P. Device keys.** Built with ADR 0028 step 1, which also registers the desktop's pid.
- Scope: `/device/v1/register`, signed device requests, and revocation on the Devices page.
- Tests:
  - `adr0030_device_requests_signed_by_another_key_are_refused`
  - `adr0030_a_revoked_computer_is_refused`
  - `adr0030_the_device_key_is_root_only`

**C1. A usable chat, sealed end to end.** On the development host and the VM only.
- Scope:
  - `glass-crypto` and its vectors.
  - `glass-managed::chat`.
  - The console's device chat routes and state.
  - In glassd: member keys, the sync thread and the store.
  - Actions: `chat.list`, `chat.read`, `chat.post`, `chat.create`.
  - DMs, groups and named channels, with people fixed when the conversation is created.
  - One computer per person. A second is refused, with the reason, until C2.
  - Every epoch sealed to a development compliance key.
  - `[[app]]` tiers.
  - `app.status` waits.
  - `apps/chat`, with `bridge.call` only.
  - The default policy.
- Tests in `glass-crypto`:
  - `hpke_opens_the_cfrg_vectors_for_p256_sha256_aes256gcm`
  - `a_changed_header_or_ciphertext_does_not_open`
  - `the_newest_epoch_key_opens_every_earlier_one`
- Console tests:
  - `gt3_the_console_never_holds_chat_plaintext`.
    - Two device clients post a canary message in a conversation that has a canary name.
    - The test searches every file in the state folder, the serialized `State` and the console's log output.
    - It looks for the canary as text, base64 and hex, and for the epoch key's bytes. None may be found.
  - `cf91_a_post_by_someone_not_in_the_conversation_is_refused`
  - `cf91_a_post_signed_by_another_member_key_is_refused`
  - `cf91_a_stale_epoch_message_is_refused`
  - `cf91_an_epoch_without_a_compliance_envelope_is_refused`
  - `cf91_a_post_wakes_waiting_computers_within_a_second`
- Runtime tests, in `crates/svc/runtime/tests/chat.rs`, with a fake `ChatLink`:
  - `cf91_agents_read_chat_at_auto_and_each_read_is_audited_without_text`
  - `cf91_an_agents_chat_post_asks_and_a_decline_is_a_readable_result`. The result is `is_error` with "declined", audited Denied, and the fake saw no post.
  - `cf91_a_group_rule_making_chat_post_never_is_a_readable_refusal`. The refusal names the rule, is audited Blocked, and nothing is posted.
  - `cf91_the_chat_app_posts_what_the_person_typed_without_asking`
  - `cf91_a_group_rule_makes_the_chat_app_ask_too`
  - `au10_an_agents_message_carries_by_agent_for_user_inside_the_seal`
  - `cf91_without_the_console_a_post_is_a_readable_refusal_and_not_queued`
- glassd: `cf91_member_keys_are_root_only_in_the_state_dir`, plus the new probe check.
- End to end, as built: the console's `cf91_two_computers_chat_through_the_consoles_device_api` runs two computers' chat stores through its real device API and `ConsoleClient`. A local person on the development host is one login, so the second person is a colleague scripted with the same code (`crates/apps/chat/examples/colleague.rs`) rather than a second glassd.
- Playwright, in `apps/e2e/chat.e2e.ts` (the console suite: glass-console, an enrolled glassd, the bridge, and the colleague):
  - two people message each other, live: a colleague's message on screen in under 1 s (113 ms on loopback, `docs/dev/performance.md`);
  - an agent's post waits on the desktop and shows its by-line;
  - a channel made in Chat, read on the colleague's computer, its name never on the console;
  - Chat keeps nothing in the page's storage (localStorage, IndexedDB, caches);
  - and in `perf.spec.ts`, Chat's cold launch and memory against UI-7.
- `app.status` waits: `cf91_chat_hears_of_a_new_message_without_reading_it` in `crates/svc/runtime/tests/chat.rs`.

**C2. Several computers, membership, presence.** Pilot gate, part one.
- Scope:
  - Several member keys per person.
  - Gap filling for new and replacement computers (CF-41).
  - Add, remove and leave, with the signed chain.
  - New epochs on removal, revocation and leaving the org.
  - Notices of new computers.
  - IT revokes a lost computer's keys.
  - Presence for lowered tiers.
  - `chat.members` and `chat.delete`.
- Presence needs the desktop, which the development host doesn't have.
  - glassd gains a development-only presence stub. It is guarded exactly like `sandbox.allow_insecure`: refused with jails, never set in a shipped config.
  - The runtime test harness fakes presence per call.
  - Without this, the C1 Playwright suite would fall to Ask once C2 lands.
- Tests:
  - `cf41_a_replacement_computer_reads_its_history_once_another_member_is_online`
  - `cf91_a_removed_member_cannot_read_later_messages`
  - `cf91_two_computers_rotating_at_once_agree_on_one_epoch`
  - `cf91_a_member_the_console_adds_without_a_members_signature_gets_no_key`, using a forged sync answer.
  - `cf91_every_key_given_away_is_audited_on_the_computer_that_gave_it`
  - `cf91_a_chat_app_post_without_the_persons_input_asks`, with presence not faked.
  - `cf91_the_presence_stub_is_refused_with_jails`

**C3. Compliance in IT's hands.** Pilot gate, part two.
- Scope:
  - The console's Chat page.
  - The compliance key, made and proven in the browser.
  - The Chat release.
  - Chat off until a key exists.
  - Retention on the console and on computers.
  - Holds.
  - Export, and decryption in the browser to JSON and RSMF, with a custody record.
  - The custody log.
  - `seal.ts`, with the shared vectors.
- Playwright tests:
  - `cf91_the_compliance_key_is_made_in_the_browser_and_never_uploaded`. Every request during key making is recorded, and none carries the private half in any encoding.
  - `cf65_an_export_decrypts_in_the_browser_with_a_custody_record`. It finds the canary, and the manifest's hashes match.
  - `cf64_a_held_message_survives_retention_and_its_senders_delete`
- Console tests:
  - `cf61_retention_deletes_unheld_ciphertext_and_its_envelopes`
  - `cf64_a_hold_blocks_deletion_until_released`
  - `cf65_the_export_bundle_holds_no_plaintext`
- Cross-language: `rust_seals_open_in_typescript_and_back`.

**C4. Like Slack.**
- **Public channels and #general.**
  - There is an org conversation that everyone is in. Its key is sealed to every member key.
  - Each public channel's epoch keys are sealed under it, as well as to IT.
  - So anyone in the org can read and join public channels without waiting on a member.
  - When a person leaves the org, that means one new org key: about 15,000 envelopes for 10,000 people.
- Browse and join, threads, reactions, edits, mentions, and unread counts across computers.
- `chat.search`.
- Notifications (CF-71).
- Labels for chat: `internal`, raised per conversation.
- Files, as sealed blobs.

**M1. Two people, sealed media.** Development only.
- Scope:
  - `glass-sfu`: str0m, ICE-lite, UDP, RTP mode.
  - The console's room routes.
  - `apps/meet`.
  - `glass-app-meet`: `meet.join` (offer in; answer, key and index out), `meet.keys` and `meet.leave`, all as calls that are never cached.
  - The data channel.
  - SFrame in a worker.
  - Calendar's meeting flag, invite and Join.
  - `meet.list`.
- The first M1 test checks that the engine has `RTCRtpScriptTransform` or `createEncodedStreams`.
- Playwright tests, with the engine's fake camera and microphone:
  - `cf21_two_people_see_and_hear_each_other`
  - `cf21_a_participant_without_the_meeting_key_decodes_nothing`. A test-only participant joins without the key; its `framesDecoded` stays at 0 while the others' climb.
  - `ui7_joining_takes_under_two_seconds`
  - `cf21_the_page_never_stores_the_media_key`
- Relay tests:
  - `cf21_the_relay_forwards_only_to_participants_the_console_admitted`
  - `cf21_the_relay_never_writes_media`
- Runtime test: `cf21_agents_cannot_join_meetings` (Never, as a readable refusal).
- Calendar tests:
  - `cf21_a_meeting_invitation_refuses_people_outside_the_org`
  - `cf21_the_media_key_is_never_sealed_to_the_compliance_key`

**M2. Twenty-five people.**
- Scope:
  - Simulcast in three layers, chosen per viewer.
  - The active speaker, and the grid.
  - A new media key when someone leaves.
  - A 26th person refused.
  - Screen sharing, once the desktop can share its screen (UI-2, CF-20).
  - Budgets for joining and for glass-to-glass delay.
- Tests:
  - `cf21_twenty_five_people_with_fake_media_stay_within_budget`
  - `cf21_someone_who_left_cannot_decrypt_after_the_new_key`
  - `cf20_cf21_screen_sharing_asks_on_the_desktop_first`

**M3. The relay in production** (GT-21, AWS). TCP on port 443 for networks that block UDP, regions, health checks, and a read-only image.

## Consequences
- **The device contract grows.** It gains device keys and signed requests, the chat routes, `DocKind::Chat` and the meeting routes.
  - New wire fields take `#[serde(default)]`.
  - The sync answer already lets unknown fields through.
- **Policy gains `[[app]]`.** `PolicyDocument` refuses unknown fields. So computers must run a glassd that knows `[[app]]` before the console publishes one, as for `[mail]`.
- **The app socket and manifests change.** `app.status` grows `wait_secs` and `since`, and manifests gain `media`. ADR 0024's list of methods and checks changes with them.
- **New code:**
  - `glass-crypto` (lib).
  - `glass-app-chat` and `glass-app-meet` (apps).
  - `glass-sfu` (bin, our servers only).
  - The web apps `apps/chat` and `apps/meet`, and the console's Chat page.
  - `openssl` becomes a direct workspace dependency.
- **The audit log gains four event kinds.** Their OCSF mapping comes with AU-2's.
- **Our cloud now stores customer chat,** as ciphertext with metadata. The trust center and GT-3's wording must say that precisely. The storage is ours to pay for.
- **Archiving to Smarsh or Global Relay (AD-16) can't happen on our side.** It needs something on the customer's side that holds the compliance key. That comes later.
- **Mail can use `[[app]]` and presence** to stop asking for every send the person typed (ADR 0024).
- **`docs/prd-traceability.md`:** CF-91 and CF-21 move to Partial as slices land.

## Licenses (checked 2026-10-19)
- **In the tree, read from the local registry (`~/.cargo/registry`):**
  - `openssl` 0.10.81, Apache-2.0. `openssl-sys` 0.9.117, MIT.
  - They have `symm::encrypt_aead`, HKDF on `PkeyCtx`, `Deriver`, EC keys and `rand_bytes`, and no HPKE.
  - `ed25519-compact` 2.6.0, MIT.
- **Not in the tree. Read from crates.io's index and API over the network, not from local metadata:**
  - `str0m` 0.24.1 is MIT OR Apache-2.0.
  - Its dependency tree with the `openssl` backend has nothing outside `deny.toml`'s list.
    - I resolved it roughly, at latest versions: 103 crates.
    - `memchr` is Unlicense OR MIT, and `unicode-ident` adds Unicode-3.0. Both pass.
  - Its `aws-lc-rs` backend would pass too: `aws-lc-sys` 0.46.0 is a combination of ISC, Apache-2.0, MIT and BSD-3-Clause. But it is a second crypto library, so it is not used.
  - `openmls` 0.9.1 is MIT.
  - `openmls_rust_crypto` 0.6.0 and `openmls_libcrux_crypto` 0.4.0 each pull `hpke-rs` 0.8.0, `hpke-rs-crypto`, and `hpke-rs-rust-crypto` or `hpke-rs-libcrux`. All four are MPL-2.0, and refused.
  - `webrtc` 0.21.1 is MIT or Apache-2.0, and `mediasoup` 0.31.0 is ISC. Top level only; their trees were not walked.
- **Read in str0m 0.24.1's published source:** `RtcConfig::set_ice_lite`, RTP mode (`set_rtp_mode`), TCP candidates (`TcpType`), and simulcast.
- **Read in the CFRG's HPKE vector file:** a base-mode vector for (0x0010, 0x0001, 0x0002), with 257 encryptions.
- **Not verified:**
  - `cargo deny check` itself, which needs the crates in `Cargo.lock`.
  - Exact resolved versions.
  - Which encoded-transform API the device's engine has.
  - WebCrypto X25519 support in IT's browsers. That uncertainty is one reason for P-256.
  - Whether the engine can capture our desktop's screen.
  - The base system's OpenSSL version on 15.1. Any 3.x has what this design uses.

## Proposed defaults (owner may change)
1. **Our servers can add a computer to a person, and it shows.** Before
   sign-in through the org's identity provider (CF-1), a compromised
   console, or root on an enrolled computer with the enrollment token,
   could register a computer as someone and receive their history. V1
   makes that visible (the person's other computers and every
   conversation say so, the computer that gives keys away audits it,
   every computer shows the compliance fingerprint) rather than
   impossible. Then member keys vouched for by the identity provider
   with CF-1; an IT-held approval for new computers as an org setting
   later.
2. **IT's key doesn't hand history to new computers.** History comes from
   other members' computers; a conversation nobody else ever opens again
   stays readable through IT's export only. The compliance key is for
   retention, holds and eDiscovery, nothing routine.

