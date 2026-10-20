# 0037. Glasshouse Mail: a mox server per org on AWS, sending through SES, reached through the same mail actions

Status: **Accepted** (2026-10-20; the owner handed the choices over: "do
what we need to do"). PRD EM-1 to EM-4 (moved into the pilot
release by the owner on 2026-10-20), IN-1, IN-2, NV-3, AU-10, CF-61 to
CF-65, DP-4, DP-30, GT-1, GT-3, GT-21. It answers the PRD's open question
"Hosted mail (EM): run our own mail servers on AWS, or a mail partner
behind our console?". Builds on:
- ADR 0018: one outbound HTTP and TLS stack, SigV4 already written.
- ADR 0023: the console, signed releases, the directory release, `[mail]`.
- ADR 0024: apps are principals.
- ADR 0030: device keys (slice P), the compliance key, `glass-crypto`,
  `[[app]]` tiers with presence.
- ADR 0033: what the console may hold, said plainly.

EM0 (the mail actions over IMAP) is being built. EM1 needs ADR 0030's
slice P (device keys) and cells on our AWS account; EM2 is the pilot gate.
The choices taken are at the end, with the four things no code can supply:
the starter domain, the Spamhaus subscription, the on-call, and SES's
production access.

## Context
The owner moved EM-1 to EM-4 into the pilot release "for all the people
starting fresh who don't have Gmail or M365 email for their teams":
- **EM-1.** A mailbox for everyone in the directory, on the org's own
  domain. The console shows the DNS records and checks them live. A new
  org can start on an address under a Glasshouse domain and move later.
- **EM-2.** SPF, DKIM and DMARC set up; incoming mail filtered for spam
  and malware; sending limits that protect the domain's reputation.
- **EM-3.** IMAP and SMTP, then JMAP, so phones work. The Mail app
  behaves the same on any provider.
- **EM-4.** Aliases, groups, shared mailboxes, retention, legal hold, and
  import from an old provider.

**Who it is for.** Teams with no email at all. Not the regulated
beachhead: banks and insurers already run Microsoft 365, and IN-1 keeps
their mail there, out of our servers.

What exists today:
- **The mail actions** (`glass-app-mail`): `mail.account`, `mail.list`,
  `mail.read` (Auto, read only), `mail.draft` (Auto, undoable) and
  `mail.send` (Ask, irreversible, up to 50 messages in one approval).
- **The store is files in the person's home** (`~/Mail/Inbox`, `Drafts`,
  `Sent`). Nothing leaves the computer. `mail.account` says so: "Sending
  and receiving aren't connected to it yet".
- **`[mail]` in policy** (NV-3, ADR 0023): `imap`, `microsoft-365` or
  `google-workspace`, with hosts and an address template. Credentials
  are never in policy. The console's Mail page edits it.
- **The directory release** gives every computer the org's people with
  their addresses. Mail's `From:` comes from it, never from the request.
- **The Mail app** (`apps/mail`) knows only those actions. It already
  shows an agent's by-line from the `X-Glasshouse-Attribution` header
  (AU-10).
- **ADR 0030** gives each computer a device key (slice P, not built) and
  each org a compliance key IT keeps, with HPKE sealing in `glass-crypto`.

What hosting mail forces, checked at the sources listed at the end:
- **Delivery is a reputation business.** Gmail asks every sender for SPF
  or DKIM, forward and reverse DNS, TLS, and a spam rate below 0.3%.
  Senders of more than 5,000 a day also need DMARC and an aligned
  `From:` domain.
- **DMARC is now RFC 9989** (May 2026), with RFC 9990 and 9991 for
  reports. It replaces RFC 7489.
- **EC2 blocks outbound port 25 unless AWS allowlists it.** So a mail
  server on EC2 can't deliver directly by default.
- **New SES accounts start in a sandbox, per region.** Verified
  recipients only, 200 messages a day, one a second. AWS gives a first
  answer to a request for production access within 96 hours.
- **GT-3 says our cloud holds only licensing, policy and telemetry
  metadata.** ADR 0030 already widened that to chat ciphertext. Hosted
  mail is content our servers can read.
- **Amazon WorkMail is closed.** It takes no new customers since
  2026-04-30 and ends on 2027-03-31.

## What runs the mail servers
Four candidates, checked at their own sources on 2026-10-20. A dagger (†)
marks what is widely known but whose page I did not open for this ADR.

| | mox 0.0.17 (Go) | Stalwart (Rust) | Postfix + Dovecot + rspamd + ClamAV | Our own, in Rust |
|---|---|---|---|---|
| SMTP in and out | Yes | Yes | Postfix | To build. `smtp-proto`, `mail-auth`, `mail-parser` help |
| IMAP | IMAP4rev2, CONDSTORE, QRESYNC | IMAP4rev2 | Dovecot | To build: the biggest part (mox's IMAP server and store are about 19,000 lines) |
| JMAP | Roadmap | Mail, Sieve, WebSocket, blobs, quotas | Not checked | To build |
| SPF, DKIM, DMARC | Yes, and DMARC reports | Yes, and ARC | rspamd † | `mail-auth` |
| MTA-STS, TLS-RPT, DANE | Yes, in and out | Yes | † | To build |
| Spam | Per-person reputation and Bayesian learning, DNSBL for unknown senders, slow-down | Rules, statistics, LLM, DNSBL, Pyzor, greylisting | rspamd † | To build |
| Malware | None | Not listed | ClamAV through rspamd (INSTREAM) | ClamAV over its socket |
| Per-person send limits | `MaxOutgoingMessagesPerDay` (1,000), `MaxFirstTimeRecipientsPerDay` (200) | Rate limiting | rspamd † | To build |
| Aliases and lists | Local members only. Outside members: roadmap | † | Postfix virtual aliases, local or remote | To build |
| Shared mailboxes (IMAP ACL) | Roadmap | † | Dovecot ACL plugin | To build |
| Retention | None (only the Rejects mailbox is cleaned) | † | Dovecot `autoexpunge` | To build |
| Legal hold, journaling | None | Undelete: enterprise only † | Postfix `always_bcc` and BCC maps; Dovecot `lazy_expunge` | To build |
| Import | mbox and Maildir | † | `doveadm` import and sync † | To build |
| Sign-in | Passwords (SCRAM), TLS client public keys (SASL EXTERNAL). OAuth: roadmap | † | Dovecot OAuth2: OAUTHBEARER, XOAUTH2, introspection or local JWT | To build |
| Many orgs | Many domains on one machine | Multi-tenancy: enterprise only | Virtual domains † | To build |
| Admin API | The web admin's JSON API (97 functions), not promised stable | Web admin † | Config files and lookup tables we generate | Ours |
| Scale | One machine. No backup MX | Clustering, S3 and SQL stores | Known at scale † | Ours |
| Language | Go | Rust | C (Postfix, Dovecot, ClamAV), C and Lua (rspamd) | Rust |
| License | MIT; one data file MPL-2.0; every module it uses is permissive | AGPL-3.0, or the proprietary SELv2 | EPL-2.0 or IPL-1.0; MIT and LGPL-2.1; Apache-2.0; GPL-2.0 | Ours, on Apache-2.0/MIT crates |
| Programs per org | One | One | Four, plus the lookup tables | One |
| Size | About 98,000 lines of Go, and 30,000 of tests | | | Of mox's order |

Why not the others:
- **Stalwart is the best fit on features and is ruled out by its
  licenses.**
  - Its open edition is AGPL-3.0. Our rule (`docs/dev/dependencies.md`)
    refuses "any license with source-disclosure or network-use terms", and
    AGPL's §13 is exactly that.
    - Running it unmodified obliges us to nothing.
    - But the first patch we carry obliges us to offer "all users
      interacting with it remotely through a computer network" the
      complete source of our version, free, from a network server. That
      means everyone in every org who reads mail over IMAP or JMAP. A
      security fix we carry before upstream is enough to trigger it.
    - Anything we link into it becomes AGPL too. Separate programs that
      talk to it over the network stay ours (§5's "aggregate").
    - Running it in a customer's AWS account (placement C below) conveys
      it. Then the whole AGPL applies: the source offer for our version
      and everything combined with it.
  - The way out is the Stalwart Enterprise License (SELv2). It is
    proprietary, needs a paid subscription and a license key, allows
    changes "solely for Licensee's internal business use", and forbids
    "reselling, leasing, sublicensing, or otherwise redistributing the
    Software". So it can't go into a customer's account under our name;
    each customer would need its own license. Multi-tenancy is enterprise
    only (its own page says so), and undelete appears to be (†).
- **Postfix, Dovecot and rspamd have everything EM-4 needs, at four C
  programs per deployment.**
  - Journaling, holds (`lazy_expunge`), retention (`autoexpunge`), ACL
    shared mailboxes, OAuth, inline ClamAV and remote aliases are all
    there.
  - The cost is the glue we would own: generating four programs'
    configuration and lookup tables per org, per-domain DKIM, rate
    limits, and patching four C code bases.
  - ClamAV needs its signatures in memory, so it is shared, not per org.
  - Their licenses don't stop a hosted service. In a customer's account
    they ship, unmodified, with a source offer, as ADR 0027 allows for
    LGPL and GPL-2.0 libraries. EPL-2.0 is new to us.
  - This is the fallback if mox fails us. Decision 1 puts the server
    behind a trait so it can be swapped.
- **Our own server in Rust** would be of mox's size, by my estimate about
  100,000 lines for SMTP, IMAP, the store, the queue and filtering. That is years for a small
  team, not a pilot.
- **A mail partner behind our console.** WorkMail is closed. AWS points
  its WorkMail customers to Kopano Cloud, Zoho Mail and Zoom Mail. I did
  not evaluate their provisioning APIs or data terms. A partner would put
  content with a fourth party, under the same GT-3 problem, behind an
  IN-1-style provider link. It is worth a look only if the owner won't
  take on the operations commitment (choice 9).
- **Selling Microsoft 365 or Google Workspace** answers a different
  request. The owner asked for mail for teams that don't want either.

## Decision

### 1. mox, one per org: a mail cell
- **A cell is one mox for one org**, on our AWS (GT-21), at a version we
  pin and build from source (Go's module proxy and checksum database).
  - It has its own volume, encrypted with the org's own KMS key
    (decision 9).
  - It has its own public IPv4 address, for MX, IMAP and submission.
  - It has two listeners; mox enables each service per listener. The
    public one serves SMTP, submission, IMAP, autoconfig and the MTA-STS
    policy, and nothing else: webmail, the account pages, the admin pages
    and the webapi are off there. The private one serves the admin API to
    the console. So nobody can set a password, add a key or change a
    webhook around the console.
  - Its host name is `<org-id>.mx.<mail host domain>`. That domain is
    ours. So moving a cell, even to the customer's account, changes only
    our record, never theirs.
- **Why one org per cell.** One key, one volume and one process per org.
  So a deleted org is a deleted volume and key. Moving an org into its
  own AWS account moves one unit. A runaway account can only fill its
  own org's disk. mox suggests a machine of its own and 512 MB of memory,
  and advises against Docker. So a cell is a small EC2 instance.
- **Why mox.**
  - MIT, and every module it uses is permissive, so it passes our rule
    hosted or shipped.
  - Go is memory safe. That matters for a server that parses hostile mail
    from the internet (the PRD's risk table asks memory-safe languages
    for new daemons).
  - One program does SMTP, IMAP, SPF, DKIM, DMARC, MTA-STS, TLS-RPT,
    junk filtering, per-person send limits, autoconfig and Apple
    profiles. It comes with a test server, `mox localserve`.
- **What mox lacks, and what we do about each.** Each change goes
  upstream first. MIT lets us carry a fork if one is refused. I estimate
  each is small next to mox's 98,000 lines of code (that count is
  measured; the sizes are not).

  | Gap | Needed for | Until it lands | The change |
  |---|---|---|---|
  | M1. A header on a transport | EM-2: the SES tenant (decision 2) | No pilot org | Add configured headers to a transport's submissions |
  | M2. An admin method to add an account's TLS public key | EM-1, EM-3: glassd signs in with a key (decision 5) | EM1 waits for it. Only the account's own API has `TLSPublicKeyAdd`, and using it needs the account's password | Expose it in the admin API |
  | M3. A journal: a copy of every message delivered or submitted | CF-64, CF-65: holds and export | Holds and export are off | A per-account journal sink |
  | M4. A hook before delivery | EM-2: malware | Scanned after delivery and moved to Quarantine (decision 2) | A content hook, or the milter support on mox's roadmap |
  | M5. IMAP ACL | EM-4: shared mailboxes on phones | Shared mailboxes are opened by glassd only (decision 4) | On mox's roadmap |
  | M6. Outside addresses in aliases | EM-4: groups with outside members | Refused, with the reason | On mox's roadmap |
  | M7. Several passwords per account | EM-3: one password per phone | One password per person (decision 5) | Later |
  | M8. JMAP, OAuth | EM-3's "then JMAP"; sign-in through the org's directory | IMAP, SMTP and keys | On mox's roadmap |

- **Behind a trait.** The console drives cells through `MailHost`, in a
  new svc crate `glass-mail-host`: create the org, domains, mailboxes,
  aliases, limits, keys, retention, holds. One implementation speaks to
  mox's admin API. A fake serves tests. Postfix and Dovecot would be a
  second implementation, not a redesign.
- **Our own code on the cell is one binary, `glass-mail-cell`** (Rust,
  our servers only, never in the image, like `glass-sfu`). It hears mox's
  webhooks on loopback, scans, seals the journal, applies retention and
  runs imports.

### 2. Out through SES, in straight to the cell (EM-2)
- **Everything a cell sends goes through SES.** That includes what mox
  makes itself: bounces, DMARC and TLS reports.
  - mox's global `Routes` send all of it to SES's SMTP endpoint
    (`email-smtp.<region>.amazonaws.com`), with SMTP credentials scoped
    to that org's identity.
  - EC2's port 25 block then never matters, and SES's sending reputation,
    feedback loops and Trust & Safety are AWS's job.
- **Each org's domain is an SES identity.**
  - **SES signs DKIM** (Easy DKIM, three CNAMEs, 2048-bit RSA), and mox
    signs nothing.
    - SES says not to sign `Message-ID`, `Date`, `Return-Path` or
      `Bounces-To` before it. I don't know mox's signed-header list. So
      the signer is the last hop.
    - SES also adds its own `d=amazonses.com` signature. That does no harm.
  - **A custom MAIL FROM**, `bounce.<domain>`, makes SPF align for DMARC.
    It needs exactly one MX, to SES's feedback host, and its own SPF.
- **Each org is an SES tenant**, with the Standard reputation policy.
  - When an org's bounces or complaints cross SES's line, SES pauses that
    org, not our account. It tells us through EventBridge, and the
    console's Mail page shows it.
  - Tenants are per region, 10,000 per account by default.
  - A tenant needs `X-SES-TENANT` on each SMTP message. mox can't add
    headers on a transport (gap M1), so no org reaches a pilot before M1
    lands.
  - SES charges per tenant per month. The pricing page I fetched didn't
    show the price.
- **Limits per person per day** are mox's, set by the console.
  - On the org's own domain: 1,000 messages and 200 first-time
    recipients (mox's defaults).
  - On the starter domain: stricter, 200 messages and 50 first-time
    recipients. The numbers are choice 5.
  - An agent's sends count like the person's. `mail.send` is still Ask,
    50 at most per approval.
  - A send refused for a limit is a tool result the agent and the person
    can read: "You've sent 200 messages today; the limit resets at
    14:02." It is never a protocol error.
- **Abuse.** When SES pauses a tenant, or a person's limits trip
  repeatedly, the console holds that person's outgoing queue (mox's hold
  rules) and tells IT. Releasing it is IT's call, and is audited on the
  console.
- **Inbound: the org's MX points straight at its cell.**
  - mox's junk filter learns per person from SPF and DKIM-verified
    domains and the sending IP. It slows down first-time and doubtful
    senders, and consults DNSBLs when it has no reputation yet.
  - Why not SES Mail Manager in front, with its virus and spam add-ons
    (Trend Micro, Vade, Spamhaus)?
    - mox would then see the relay's IP. mox has no trusted-relay setting,
      and ARC is only on its roadmap. Its reputation and SPF checks would
      break.
    - Every customer's MX would name an `amazonaws.com` host. Moving
      endpoints would mean asking every org to edit DNS.
    - An endpoint costs $50 a month.
- **Spam starts untrained.** A new person's filter has no history. The
  pilot measures spam that reaches inboxes and "Not junk" rescues. The
  DNSBLs we use for a commercial service are choice 10.
- **Malware: scanned on arrival, moved before anyone reads it.**
  - mox's incoming webhook tells `glass-mail-cell` of each delivery.
  - It fetches the message over IMAP on loopback, signed in to that
    mailbox with a service key of its own (a TLS client key, gap M2).
    mox's webapi would need each account's password, and these accounts
    may have none. IMAP also works the same against Dovecot.
  - It streams the message to a shared clamd pool in the region (ClamAV's
    INSTREAM).
  - It refuses executable and macro-carrying attachment types by name and
    content.
  - A hit is moved to a `Quarantine` mailbox (IMAP MOVE) with a note.
    IT can release it from the console.
  - **The gap:** the message sits in the Inbox for the seconds between
    delivery and the move, and a phone polling at that moment can fetch
    it. Gap M4 closes that by scanning before delivery.
  - The Mail app and `mail.read` never show a message the scanner hasn't
    passed. The scanner marks each message it passes with a keyword
    (IMAP STORE), and glassd hides Inbox mail without it, so agents never
    see one.

### 3. Domains and the console (EM-1)
**Start.** IT picks "Glasshouse Mail" on the console's Mail page, beside
Microsoft 365, Google Workspace and IMAP. The console then:
- creates the cell and the starter domain `<org-slug>.<starter domain>`
  in our own DNS. We publish every record ourselves there, with DMARC at
  `p=reject` from the first day, since we know the only sender.
- gives everyone in the directory a mailbox, `<login>@<org-slug>.<starter
  domain>`, and publishes a directory release with those addresses. So
  Mail's `From:` changes with no step on the computer.
- publishes `[mail] service = "glasshouse"` (decision 6).

The starter domain shares one parent domain among new orgs. That is why
its limits are stricter, why each org is its own tenant, and why the
parent must never be our company's own mail domain. Its name waits on
the product name (choice 4).

**Adding the org's domain.** IT types `acme.com`. The console shows every
record with its state, checked live:

| Purpose | Name | Type | Value |
|---|---|---|---|
| Ownership, before any cell accepts mail for it | `_glasshouse.acme.com` | TXT | `org=<id> token=<random>` |
| Mail for the domain reaches the cell | `acme.com` | MX | `10 <org-id>.mx.<mail host domain>` |
| SPF | `acme.com` | TXT | `v=spf1 include:amazonses.com ~all` |
| Bounces (SES custom MAIL FROM, exactly one MX) | `bounce.acme.com` | MX, TXT | `10 feedback-smtp.<region>.amazonses.com`; `v=spf1 include:amazonses.com ~all` |
| DKIM | three names under `_domainkey.acme.com` | CNAME | the three values SES gives the identity |
| DMARC (RFC 9989) | `_dmarc.acme.com` | TXT | `v=DMARC1; p=none; rua=mailto:dmarc-reports@acme.com`, then `p=quarantine` |
| MTA-STS (RFC 8461) | `_mta-sts.acme.com`; `mta-sts.acme.com` | TXT; CNAME | `v=STSv1; id=<n>`; the cell, which serves the policy |
| TLS reports (RFC 8460) | `_smtp._tls.acme.com` | TXT | `v=TLSRPTv1; rua=mailto:tls-reports@acme.com` |
| The name phones see | `mail.acme.com` | CNAME | the cell (mox's client settings domain) |
| Mail app setup | `autoconfig.acme.com`; `_imaps._tcp`, `_submissions._tcp`, `_autodiscover._tcp` | CNAME; SRV | the cell; `0 1 993 mail.acme.com`, `0 1 465 mail.acme.com` (RFC 6186, 8314) |

- **Each record is Found, Missing, Wrong** (the page shows what is there
  instead) **or Conflict.** A second SPF record is the Conflict example:
  RFC 7208 makes two a permanent error, so every receiver's check fails.
  An old provider's MX is another.
- **Checked live.** While the page is open, the console asks the domain's
  authoritative servers every 10 seconds, never a cache. After that it
  checks hourly, and nightly once all records pass.
- **Drift is an alert.** A record that passed and then breaks (someone
  edits SPF for a newsletter tool) notifies IT and shows on the Mail
  page.
- **Order.**
  - Ownership first. A domain is one org's only.
  - DKIM, bounces and SPF next, so mail can leave signed before it
    arrives.
  - MX last.
  - MTA-STS starts in `testing` and the console offers `enforce` once a
    week of TLS reports is clean.
  - DMARC starts at `p=none` and the console offers `quarantine` after 14
    days of clean aggregate reports. mox receives and summarizes the
    reports, and the console reads the summaries.
  - mox notes that SRV and autodiscover records are only safe with
    DNSSEC. The CNAMEs that carry TLS are the ones that matter.
- **DNS checks live in `glass-mail-host`**, on `hickory-resolver` (MIT OR
  Apache-2.0), a console-only dependency that `tools/check-layers.py`
  will keep there.

**Moving to the org's domain.** Once MX, SPF, DKIM, bounces and DMARC pass,
IT presses "Make acme.com the address":
- every mailbox gets `<login>@acme.com` as its primary address, and the
  directory release carries it;
- the starter address stays as an alias for 12 months, then bounces with
  the new address in the message;
- phones keep working, since mox accepts any of an account's addresses
  for login;
- mail no longer leaves as the starter address.

**Leaving.** Each mailbox exports as mbox (the console builds the bundle
on the cell; IT downloads it). Pointing the MX elsewhere stops delivery.
The cell is kept 30 days after an org leaves, then its volume and key are
deleted.

### 4. People, groups and shared mailboxes (EM-4)
- **The directory is the source** (AD-1, SCIM later).
  - A new person gets a mailbox.
  - A person who leaves can't sign in, at once (mox's `LoginDisabled`).
    The console also removes their address from the account
    (`AddressRemove`), so new mail to it bounces; `LoginDisabled` alone
    still accepts deliveries. IT can instead point the address at a
    colleague as an alias.
  - Their mailbox is kept for IT's export, under retention. IT can turn
    it into a shared mailbox for a colleague.
- **Groups become aliases.**
  - A directory group IT marks as a mail group becomes an alias whose
    members are the group's people.
  - `PostPublic` decides whether outsiders may write to it.
  - `AllowMsgFrom` lets members send as it.
  - Extra aliases per person (`sales@`, `priya.s@`) are set on the
    console.
  - Outside members are refused, with the reason, until gap M6.
- **Shared mailboxes** (`support@`) are mailboxes with no person and a
  directory group as members.
  - Every member's computer has a key registered on it (decision 5), so
    the Mail app lists it under the person's own.
  - Mail actions gain an optional `mailbox`, the person's own by default.
  - An agent reaches a shared mailbox only when the person it works for
    is a member, and only at the person's tiers.
  - Phones don't see shared mailboxes until IMAP ACL (gap M5).

### 5. Signing in (EM-3)
- **Computers sign in with a key, never a password.**
  - glassd makes a key pair per person per computer the first time Mail
    or an agent needs mail. It lives in `/var/db/glasshouse/mail/<login>/`,
    root only, outside every jail.
  - It registers the public half with the console, signed by the device
    key (ADR 0030 slice P). The console adds it to the person's account
    as a TLS public key (gap M2).
  - glassd then signs in to IMAP and submission with TLS client
    authentication and SASL EXTERNAL.
  - Revoking a computer on the Devices page removes its keys.
  - Nothing an agent or a page can read ever holds a mail credential.
- **Phones and other mail apps use one password per person.**
  - iOS's Mail profile offers password, CRAM-MD5, NTLM, HTTP-MD5 or none,
    not OAuth.
  - Thunderbird uses OAuth only for hosts in its built-in list (Google,
    Microsoft, Fastmail, Yandex, Mail.ru, Seznam, Comcast) or added by an
    extension.
  - So generic mail apps need a password.
  - In the Mail app, "Set up a phone" calls `mail.phone`:
    - it is the person's own action, Never for agents, and needs presence;
    - the console sets a random password on the account (with mox's
      `NoCustomPassword`, people can't choose their own), passes it
      through, and keeps no copy;
    - the app shows it once, with a QR code for an Apple configuration
      profile and the settings for any other app;
    - running it again replaces the password, which signs out every
      phone.
  - One password per device needs gap M7. Until then, losing a phone
    means resetting all of them.
  - IT can turn phones off per group. Then no password is set, and only
    computers' keys can sign in.
- **Autoconfig.** mox serves Thunderbird-style autoconfig and Apple
  profiles, and the SRV records above point other apps at it.
- **The org's own directory sign-in (OAuth) and JMAP** come with mox's
  roadmap (gap M8) and with CF-1. glassd's link then changes, and the
  actions don't (decision 6).

### 6. On the computer: one set of actions, a link per provider (IN-2)
- **`MailLink`** is a trait in `glass-managed::mail`, beside `ChatLink`:
  list folders, sync a folder since a state, fetch a message, submit, and
  append to a folder.
  - `glass-app-mail` holds a `MailLink`, so it depends only on lib crates.
  - The implementations live in `glass-gateway-providers`, the one crate
    with a network stack:
    - `ImapLink`: IMAP and SMTP submission over native-tls. It serves
      Glasshouse Mail and `[mail] service = "imap"`, which ends NV-3's
      "not connected".
    - `GraphLink`: Microsoft 365, IN-1's own slice.
    - `JmapLink`: later.
  - The local store stays as `LocalLink`, for development and tests.
- **The actions don't change** (IN-2): the same names, inputs, tiers and
  side effects.
  - `mail.send` stays Ask and irreversible.
  - The person's own sends from the Mail app use `[[app]]` tiers with
    presence, as `chat.post` does (ADR 0030). The app stops asking for
    what the person just typed.
  - New input fields:
    - `mailbox` (decision 4);
    - a `junk` folder, so people can rescue mail.
  - `mail.move` (Inbox, Archive, Junk, Trash) is new:
    - Notify for agents;
    - undone by moving back, through the provider's undo hook;
    - moving out of Junk trains the filter.
- **`~/Mail` stays as the cache.**
  - glassd syncs each folder into it, with CONDSTORE and QRESYNC, and
    IDLE on the Inbox.
  - Message ids stay `<folder>/<file>.eml`; the file name carries the
    folder's UIDVALIDITY and the message's UID.
  - So path labels, offline reads (DP-30) and draft undo hold as today.
  - `app.status` waits tell the Mail app of new mail.
  - Offline, `mail.send` says so and keeps the message in Drafts.
- **Sending.**
  - glassd builds the message: `From:` from the directory, a
    `Message-ID` it makes, and the `X-Glasshouse-Attribution` header
    (AU-10, choice 7).
  - It submits it, then appends it to Sent.
  - The audit record's `detail.mail` holds the Message-ID, the recipient
    count, the size and SHA-256 of what left, and the server's reply.
    Never the body.
- **Reading.**
  - `mail.read` returns text. HTML is reduced to text for agents.
  - The Mail app shows HTML the way artifacts are shown (ADR 0033): its
    own origin, a CSP sandbox with no network, remote images off.
  - Subjects and senders are untrusted text. `display_line` and the
    SDK's text-only DOM builder already cover them.
- **`[mail] service = "glasshouse"`** is a new `MailService` value. The
  console fills in the cell's hosts, so IT never types them.

### 7. Retention, holds and export (CF-61 to CF-65)
- **Retention (CF-61).**
  - Set on the console in days, per org.
  - There is no default, as for chat: mail is kept until IT sets a
    period.
  - `glass-mail-cell` deletes older mail over IMAP every night, except
    for held people.
  - Trash empties after 30 days.
- **A journal sealed to IT's compliance key.**
  - With gap M3, every message delivered to or submitted by a mailbox is
    copied to `glass-mail-cell`.
  - It seals each copy with HPKE to the org's compliance key, through
    `glass-crypto`, the same suite and code as chat.
  - It stores the result in the org's archive bucket, under the org's
    key as well.
  - Neither the cell nor the console can open the journal.
  - The journal exists only once IT has made a compliance key (ADR 0030
    §4). Mail itself works without one.
- **Legal hold (CF-64).**
  - Set on the console: people, a date range and a matter.
  - Held people's mail is skipped by retention, and their journal
    entries outlive retention until the hold is released.
  - A message a held person deletes leaves their mailbox but stays in
    the journal. This is the same choice as chat: preserved, not
    blocked. Blocking the delete itself would need another mox change.
- **eDiscovery export (CF-65).**
  - IT picks a hold or a query. The console bundles the matching sealed
    entries with a manifest of their SHA-256, signed by the org key.
  - IT's browser opens them with the compliance key file (ADR 0030 §4)
    and saves EML files in a zip, and an mbox, with a custody record.
  - Holds, releases and exports go in the console's custody log.
- **The live mailbox can't be sealed.** The cell must read mail to filter
  it, to serve phones over IMAP and to search. So the journal is what the
  compliance key protects from us, not the mailbox (decision 9).

### 8. Import from an old provider (EM-4)
- IT starts an import per person or for everyone:
  - from any IMAP server, with credentials IT enters for the job only;
  - from Microsoft 365 through `GraphLink` once IN-1 has it;
  - or from an mbox file, such as Google Takeout's.
- `glass-mail-cell` runs it on the cell, so the mail never passes the
  console.
  - It copies folder by folder with IMAP APPEND.
  - It skips Message-IDs already there, so a rerun is safe.
  - It reports counts per folder, and failures by message.
  - The source credentials are dropped when the job ends.
- The importer authenticates to the cell with a key registered for the
  job and removed after it.

### 9. Where mail lives (GT-3)
Decided (choice 2): A for the pilot release, B in EM5, C in P1. The
options, from least to most work:
- **A. Our AWS account, our key per org.** Cells in our account, each
  volume and archive under a KMS key of the org's own.
  - We can read the mail, and must say so.
  - This is what the slices build first.
- **B. Our account, the customer's key.**
  - The org creates the KMS key in its own AWS account and grants it to
    our cell's role. KMS allows that across accounts, with the key policy
    in their account and an IAM policy in ours.
  - Every use shows in both accounts' CloudTrail.
  - Revoking the grant makes the volume and archive unreadable: a
    crypto-shred they control.
  - It doesn't stop us reading mail while the grant stands.
  - Whether EBS accepts another account's key for our volumes is to be
    checked in EM5.
- **C. The cell in the customer's own AWS account.**
  - Their EC2, their SES, their keys, their bill. Our console drives it
    through a role they grant. GT-3 holds.
  - Their SES starts in the sandbox: up to 96 hours for AWS's first
    answer before mail leaves.
  - It ships the cell's software to them, so it must pass our license
    rule as shipped code: mox (MIT, with the MPL-2.0 suffix list), and
    ClamAV (GPL-2.0) unmodified with a source offer (choice 8).

Whichever is chosen:
- **GT-3's wording and the trust center change.** For orgs on Glasshouse
  Mail, in A or B, our cloud holds their mail and our servers can read it
  while delivering, filtering and serving it.
- **GT-1's scope grows.** SOC 2 and the penetration test must cover a
  system that reads customer content. That system did not exist when
  GT-1 was planned.
- **The console holds what IT needs, no content:** domains, mailboxes and
  aliases, DNS states, sending counts, SES tenant status, holds and the
  custody log. `gt3_the_console_never_holds_mail_content` proves it.

**Recommendation:** A for the pilot release, offered only to orgs with no
mail of their own. B as an option in EM5. C in P1, for anyone who needs
GT-3 to hold. Never offer Glasshouse Mail to a regulated pilot: they are
on Microsoft 365 (IN-1), and the PRD's beachhead stays clean.

### 10. Our license rule, for servers we run
`deny.toml` already covers servers that never ship (`glass-console`,
`glass-sfu`). But a third-party program we run beside our code is outside
the workspace, and `dependencies.md` speaks of "shipped code".

Proposed rule: what we run for customers, hosted or shipped, is one of
these:
- permissive;
- copyleft without network-use terms, run unmodified as its own program
  (EPL, LGPL, GPL-2.0), with a source offer when it ships;
- never AGPL, SSPL, or a license that forbids redistribution.

`docs/dev/dependencies.md` has that line (choice 8).

## Containment review
- **Agents gain no reach.** They still call `mail.*` through the task
  socket. Jails still have no network.
  - A new probe check: from a jail, the cell's IMAP and submission ports
    and the mail hosts in policy don't answer.
- **glassd gains outbound connections.**
  - IMAPS (993) and submission (465) go only to the hosts in the signed
    `[mail]` policy.
  - TLS is verified against the base system's trust store.
  - The client keys are root only. A probe check makes sure no jail can
    read `/var/db/glasshouse/mail`.
- **`mail.phone` is Never for agents** in the default policy. It needs
  presence. Its audit record never carries the password.
- **Outside mail is hostile input.**
  - HTML is shown under an artifact-style CSP sandbox and reduced to text
    for agents.
  - Display text is sanitized.
  - DP-4 (injection screening of retrieved content, not started) becomes
    urgent, since mail from strangers is the classic injection channel.
- **Limits and refusals are tool results.**
  - A limit, a paused tenant or a refused outside member is text the
    agent can read, audited Blocked or Failed.
  - A declined send sends nothing.
- **Undo.** `mail.send` stays irreversible and listed as such.
  `mail.move` is undone by moving back, with the change recorded. Drafts
  stay file snapshots.

## Building it
Every test name carries its PRD id. Slices are named EM0 to EM5;
requirement ids are EM-1 to EM-4, so a test in slice EM2 can carry
`em1_` when it proves EM-1 (the records table). Runtime and Playwright
tests run against `mox localserve` (a build-time test tool, never shipped) and a
fake `MailHost`. Nothing reaches a pilot org before EM2.

**EM0. The link, against any IMAP server.** It is useful before cells
exist, and closes NV-3's "not connected".
- Scope:
  - `MailLink` in `glass-managed`;
  - `ImapLink` in `glass-gateway-providers`;
  - the cache;
  - `mail.move` and the `junk` folder;
  - HTML in a sandboxed frame;
  - `[mail] service = "imap"` with a password the person enters once
    through `mail.connect` (the person's own action, Never for agents),
    kept root only.
- Tests:
  - `in2_mail_actions_return_the_same_shapes_on_the_local_store_and_on_imap`
  - `in2_an_agents_send_asks_and_a_decline_sends_nothing`, which checks
    mox's queue stays empty.
  - `in2_a_group_rule_making_mail_send_never_is_a_readable_refusal`
  - `au10_a_sent_message_carries_the_by_line_and_the_audit_its_sha256`
  - `dp30_offline_mail_reads_from_the_cache_and_a_send_says_why_it_waited`
  - `nv3_new_mail_reaches_the_mail_app_within_two_seconds`, in Playwright,
    with a `glass-e2e perf` budget.
  - `nv3_an_html_message_runs_no_script_and_loads_no_remote_image`
  - Probe: `mail_ports_and_keys_are_unreachable_from_a_jail`.

**EM1. A cell and the starter domain.** Needs ADR 0030 slice P (device
keys).
- Scope:
  - `glass-mail-host` with the mox implementation;
  - cells on AWS;
  - the starter domain;
  - mailboxes from the directory;
  - keys for computers (gap M2);
  - `[mail] service = "glasshouse"`;
  - the console's Mail page choice.
- Tests:
  - `em1_a_new_org_gets_a_starter_address_for_everyone_in_the_directory`
  - `em1_a_person_who_leaves_the_directory_cannot_sign_in`
  - `em1_a_computer_signs_in_with_its_key_and_a_revoked_computer_cannot`
  - `em1_the_cells_public_listener_serves_no_web_pages`
  - `em1_two_colleagues_mail_each_other_through_the_cell`, end to end.
  - `gt3_the_console_never_holds_mail_content`. A canary subject and body
    go through the cell. The test searches the console's state, files and
    logs for it as text, base64 and quoted-printable, and finds nothing.

**EM2. The org's domain, delivery and filtering.** Pilot gate.
- Scope:
  - the records table and live checks;
  - moving from the starter domain;
  - SES identities, Easy DKIM, MAIL FROM;
  - tenants (gap M1);
  - limits and the abuse hold;
  - the scanner and Quarantine;
  - DMARC and TLS report summaries on the Mail page.
- Tests:
  - `em1_each_record_shows_found_missing_wrong_or_conflict`
  - `em1_two_spf_records_are_a_conflict`
  - `em1_a_domain_another_org_proved_is_refused`
  - `em1_records_are_read_from_the_authoritative_servers`, with a fake
    nameserver that serves a stale cache a different answer.
  - `em1_moving_to_the_orgs_domain_keeps_the_starter_address_as_an_alias`
  - `em2_every_message_a_cell_sends_leaves_through_ses_with_its_tenant`
  - `em2_a_persons_daily_limit_is_a_readable_refusal_for_an_agent`
  - `em2_a_paused_tenant_shows_on_the_mail_page_and_holds_the_queue`
  - `em2_the_eicar_test_file_is_quarantined_before_mail_list_shows_it`
  - `em2_an_executable_attachment_is_quarantined`

**EM3. Phones.**
- Scope: `mail.phone`, the profile and QR code, autoconfig and SRV, and
  phones off per group.
- Tests:
  - `em3_a_phone_password_works_over_imap_and_submission_and_is_shown_once`
  - `em3_setting_up_a_phone_again_signs_out_every_phone`
  - `em3_mail_phone_is_never_for_agents`
  - `em3_with_phones_off_only_computer_keys_sign_in`
  - Manual: an iPhone and Thunderbird on a pilot domain, with
    screenshots.

**EM4. Groups, shared mailboxes, retention, holds, import.**
- Scope: aliases from groups, shared mailboxes, retention, the sealed
  journal (gap M3), holds, export, import.
- Tests:
  - `em4_a_directory_group_becomes_an_alias_with_its_members`
  - `em4_an_outside_member_is_refused_with_the_reason`
  - `em4_an_agent_reads_a_shared_mailbox_only_for_a_member`
  - `cf61_retention_deletes_mail_older_than_the_period_except_held`
  - `cf64_a_held_persons_deleted_message_stays_in_the_journal`
  - `gt3_the_journal_opens_only_with_the_compliance_key`
  - `cf65_an_export_opens_in_the_browser_with_a_custody_record`, in
    Playwright. It finds the canary, and the manifest's hashes match.
  - `em4_an_imap_import_copies_every_folder_once_and_reports_counts`
  - `em4_a_rerun_import_adds_nothing`

**EM5. Placements and later protocols.**
- Scope:
  - placement B (and checking EBS with another account's key);
  - placement C, in P1;
  - gaps M4, M5, M7 and M8 as they land upstream;
  - `JmapLink`.

## Consequences
- **We run a mail service.**
  - Postmaster and abuse addresses, an on-call rotation for delivery and
    abuse, DNSBL and feedback-loop monitoring, and answering SES Trust &
    Safety.
  - The PRD names this commitment. Choice 9 takes it on.
- **Go joins our servers.** mox is a pinned third-party program built
  from source, outside the workspace. Upgrades go through
  `mox localserve` tests, since its admin API isn't promised stable.
- **The upstream changes are our work.** Gaps M1 to M3 gate the pilot. If
  mox's author declines them, we carry a fork, and we watch the project's
  bus factor: mox was created by and is led by one developer.
- **A cell per org costs** a small instance, a public IPv4 address, a
  volume and a key per org. At scale, small orgs on the starter domain
  could share a cell, at the cost of sharing a key.
- **No backup MX.** mox has none. Senders retry for days, so an outage
  delays mail rather than losing it. The Mail app reads from its cache
  meanwhile.
- **The device contract grows.**
  - `[mail] service = "glasshouse"` and the new action fields.
    `PolicyDocument` refuses unknown values, so computers need a glassd
    that knows them before the console publishes, as with `[mail]` and
    `[[app]]`.
  - Device routes for mail keys and `mail.phone`.
- **The console gains outbound calls.**
  - `glass-console` takes `glass-gateway-providers` as a normal
    dependency, for the cells' admin API and for SES and Route 53 through
    the existing SigV4 signer. Still one HTTP and TLS stack.
  - `hickory-resolver`, for DNS, is the console's only new network
    dependency.
- **New code:**
  - `glass-mail-host` (svc, console only);
  - `glass-mail-cell` (bin, our servers only);
  - `MailLink` (lib);
  - `ImapLink` (svc);
  - the Mail page's sections;
  - the sandboxed HTML view.
- **`docs/prd-traceability.md`:** EM-1 to EM-4 move to In progress as
  slices land; NV-3's mail row with EM0; GT-3's row says what hosted mail
  holds.
- **`docs/architecture.md`:** the constraint "Our cloud services ... hold
  only licensing, policy and telemetry metadata" gains its mail
  exception.

## Licenses (checked 2026-10-20)
- **mox 0.0.17** (released 2026-08-19): MIT, read in the module's
  `LICENSE.MIT` from Go's module proxy.
  - It embeds Mozilla's Public Suffix List
    (`publicsuffix/public_suffix_list.txt`), which is MPL-2.0. That is a
    data file, unmodified.
  - Every module in its `go.mod` is permissive on pkg.go.dev:
    - BSD-3-Clause: `mjl-/adns`, `autocert`, `flate`, `golang.org/x/*`,
      `rsc.io/qr`, `google.golang.org/protobuf`;
    - MIT: `bstore`, `sconf`, `sherpa*`, `xfmt`, bbolt, xxhash, perks;
    - Apache-2.0: the Prometheus modules,
      `golang_protobuf_extensions`;
    - BSD-2-Clause: blackfriday.
- **Stalwart:** AGPL-3.0-only or `LicenseRef-SEL`, from the repository's
  `LICENSES` folder.
  - AGPL-3.0 §13 is quoted from gnu.org's text.
  - SELv2's terms are quoted from its file: a paid subscription and
    license key, changes for internal use only, no redistribution, and
    "not open source".
  - **Ruled out**, hosted and shipped.
- **Postfix:** EPL-2.0 or IPL-1.0, from its `LICENSE`. I read it in a
  GitHub mirror (`vdukhovni/postfix`); postfix.org's own pages answered,
  but I did not find the file there.
- **Dovecot:** `src/lib`, `src/auth` and `src/lib-sql` are MIT, the rest
  LGPL-2.1, from its `COPYING`.
- **rspamd:** Apache-2.0, from its `LICENSE.md`.
- **ClamAV:** GPL-2.0, from its `COPYING.txt`. It is used only as a
  separate daemon, unmodified.
- **Rust crates, from crates.io's API at the exact versions:**
  - all Apache-2.0 OR MIT: `mail-parser` 0.11.9, `mail-auth` 0.13.3,
    `mail-builder` 1.0.0, `mail-send` 0.6.2, `smtp-proto` 0.2.5,
    `jmap-client` 0.4.3;
  - MIT OR Apache-2.0: `imap-codec` 1.0.0, `imap-next` 0.3.4,
    `async-imap` 0.12.0, `hickory-resolver` 0.26.3;
  - MIT: `lettre` 0.11.23.
  - `sieve-rs` 1.0.2 is AGPL-3.0-only, and refused.
- **Not verified:**
  - `cargo deny check` itself. No builds were run, and none of these
    crates' dependency trees were walked.
  - Which IMAP and SMTP client crates `ImapLink` uses; that is decided
    in EM0.
  - Which key types mox accepts for TLS client authentication.
  - mox's DKIM signed-header list.
  - SES's price per tenant, and public IPv4 and instance costs.
  - Whether EBS accepts another account's KMS key for our volumes.
  - Stalwart's OAuth support, and the Postfix and rspamd features marked
    † in the table.
  - The partners AWS names for WorkMail customers.

## Choices made (2026-10-20)
The owner handed these over ("do what we need to do"). Each takes the
recommendation above unless it says otherwise.
1. **Server: mox, one per org.** The upstream changes M1 to M3 land before
   any pilot org. Postfix, Dovecot and rspamd stay the fallback behind
   `MailHost`.
2. **GT-3: placement A for the pilot release, B in EM5, C in P1.** Offered
   only to orgs with no mail of their own, never to a regulated pilot. The
   PRD's GT-3 now says what hosted mail holds, and the trust center will
   say the same. GT-1's SOC 2 and penetration test cover the cells,
   `glass-mail-cell` and the console's mail routes.
3. **The journal is sealed to IT's compliance key**, as chat is.
4. **The starter domain is a setting of our deployment**, with no default.
   A console without one offers only the org's own domain. It must be a
   domain we register for this alone, never our company's mail domain, and
   its name waits on the product name. EM1 can't make it up.
5. **Limits as proposed**, as settings of the deployment so the pilot can
   tune them: 1,000 messages and 200 first-time recipients a day per person
   on the org's domain; 200 and 50 on the starter domain.
6. **Phones on by default**, one password per person until per-device
   passwords (M7). IT can turn phones off per group.
7. **The by-line header stays on every message** (AU-10), outside
   recipients included.
8. **The license rule for servers we run** is in `docs/dev/dependencies.md`
   (§10). In placement C, ClamAV and the suffix list ship unmodified with a
   source offer.
9. **We run the mail service ourselves**, with what that takes:
   `postmaster@` and `abuse@` on the mail host domain read every day, a
   delivery and abuse on-call, DNSBL and feedback-loop watching, and SES
   Trust & Safety answered within its deadlines. This is a commitment of
   people, not code: no pilot org starts until the on-call exists. No mail
   partner.
10. **Spam feeds: Spamhaus, on its paid service, and no other list at
    first.**
    - Spamhaus's free public mirrors are "provided free of charge for
      non-commercial use by small and medium sized organisations", and
      "ISPs and commercial spam filter services" must subscribe to its
      Datafeed Service (its DNSBL usage terms, §1.1 and §2). A hosted mail
      service is one of those. The free mirrors also refuse queries "from
      large shared hosting environments", which a cloud network can count
      as.
    - SpamCop's list publishes no terms for commercial use. SpamCop itself
      says the list "often errs on the side of blocking mail" and
      encourages users "to tag and divert email, rather than block it
      outright". mox rejects any message a listed DNSBL names, so SpamCop
      would reject good mail. It is out.
    - mox names both lists only as examples and sets none by default.
    - Cells take Spamhaus's zone, with our subscription's key, through
      mox's `DNSBLs`. Until the subscription is bought (a purchase, outside
      this repository), cells run on mox's per-person reputation alone, and
      the pilot measures what that lets through.
    - `MonitorDNSBLs` stays empty, since cells send nothing themselves
      (§2).

**What no code supplies**, before the first pilot org on Glasshouse Mail:
- the starter domain, registered (choice 4);
- the Spamhaus subscription (choice 10);
- the on-call, and the postmaster and abuse mailboxes staffed (choice 9);
- SES production access for our account: up to 96 hours for AWS's first
  answer, asked once the account and the mail host domain exist.

## Sources
Opened for this ADR on 2026-10-20:
- Spam feeds (choice 10):
  - https://www.spamhaus.org/organization/dnsblusage.html (the DNSBL usage
    terms, §1.1 and §2)
  - https://www.spamcop.net/bl.shtml
  - https://www.xmox.nl/config/ (`DNSBLs`, `MonitorDNSBLs`, and their
    example lists)
- mox:
  - https://github.com/mjl-/mox
  - https://raw.githubusercontent.com/mjl-/mox/main/README.md
  - https://www.xmox.nl/features/
  - https://www.xmox.nl/config/
  - https://www.xmox.nl/commands/
  - https://www.xmox.nl/faq/
  - https://github.com/mjl-/mox/releases
  - https://github.com/mjl-/mox/releases/tag/v0.0.14
  - https://proxy.golang.org/github.com/mjl-/mox/@v/v0.0.17.zip (license
    files, `go.mod`, `webadmin/api.json`, `webaccount/api.json`,
    `webapi/webapi.go`, `webapisrv/server.go`, `webhook/webhook.go`, line
    counts)
  - https://pkg.go.dev/ license tabs for each module in mox's `go.mod`
- Stalwart:
  - https://github.com/stalwartlabs/stalwart
  - https://github.com/stalwartlabs/stalwart/tree/main/LICENSES
  - https://raw.githubusercontent.com/stalwartlabs/stalwart/main/LICENSES/LicenseRef-SEL.txt
  - https://stalw.art/docs/server/enterprise/
- https://www.gnu.org/licenses/agpl-3.0.txt
- Postfix:
  - https://www.postfix.org/
  - https://raw.githubusercontent.com/vdukhovni/postfix/master/postfix/LICENSE
  - https://www.postfix.org/postconf.5.html
  - https://www.postfix.org/virtual.5.html
- Dovecot:
  - https://raw.githubusercontent.com/dovecot/core/main/COPYING
  - https://doc.dovecot.org/main/core/plugins/lazy_expunge.html
  - https://doc.dovecot.org/main/core/config/auth/databases/oauth2.html
  - https://doc.dovecot.org/main/core/plugins/acl.html
- https://raw.githubusercontent.com/rspamd/rspamd/master/LICENSE.md
- https://docs.rspamd.com/modules/antivirus
- https://raw.githubusercontent.com/Cisco-Talos/clamav/main/COPYING.txt
- crates.io's API, at `https://crates.io/api/v1/crates/<name>/<version>`,
  for each crate listed above.
- https://raw.githubusercontent.com/stalwartlabs/mail-auth/main/README.md
- Amazon SES:
  - https://docs.aws.amazon.com/ses/latest/dg/tenants.html
  - https://docs.aws.amazon.com/ses/latest/dg/eb.html
  - https://docs.aws.amazon.com/ses/latest/dg/eb-ingress.html
  - https://docs.aws.amazon.com/ses/latest/dg/eb-rules.html
  - https://docs.aws.amazon.com/ses/latest/dg/eb-addons.html
  - https://docs.aws.amazon.com/ses/latest/dg/receiving-email-concepts.html
  - https://docs.aws.amazon.com/ses/latest/dg/send-email-authentication-dkim.html
  - https://docs.aws.amazon.com/ses/latest/dg/send-email-authentication-dkim-easy.html
  - https://docs.aws.amazon.com/ses/latest/dg/send-email-authentication-dkim-manual.html
  - https://docs.aws.amazon.com/ses/latest/dg/creating-identities.html
  - https://docs.aws.amazon.com/ses/latest/dg/mail-from.html
  - https://docs.aws.amazon.com/ses/latest/dg/request-production-access.html
  - https://docs.aws.amazon.com/general/latest/gr/ses.html
  - https://aws.amazon.com/ses/pricing/
- https://docs.aws.amazon.com/workmail/latest/adminguide/workmail-end-of-support.html
- https://docs.aws.amazon.com/kms/latest/developerguide/key-policy-modifying-external-accounts.html
- https://support.google.com/a/answer/81126
- https://developer.apple.com/tutorials/data/documentation/devicemanagement/mail.json
  (the Mail payload's authentication values)
- https://raw.githubusercontent.com/mozilla/releases-comm-central/master/mailnews/base/src/OAuth2Providers.sys.mjs
- RFC metadata from rfc-editor.org (`https://www.rfc-editor.org/rfc/rfc<n>.json`):
  - RFC 6186, 6409, 7208, 7489 (obsoleted by 9989 to 9991), 7628;
  - RFC 8314, 8460, 8461, 8620, 8621, 9051;
  - RFC 9989, 9990, 9991.
