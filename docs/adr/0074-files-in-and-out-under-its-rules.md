# 0074. Files between a person's own devices and their agents' computer, under IT's rules

Status: **Proposed, designed, not built** (2026-10-28). The owner: "We
also need to think about how we can move files from the host desktop app
to the glasshouse os and what types of files can go back and fourth maybe
it should be a admin setting that's the enterprise can configure…"

## Context
- **Nothing moves files in or out of a person's computer today.**
  - `glass cp` is planned in ADR 0065 §12. Dropping a file on an agent is
    on ADR 0071's list of what's missing.
  - The front has no route for files. `Ask.files` ("paths in the person's
    files to give it, as copies") exists in the API, but nothing reads or
    sends it.
  - What an agent makes is seen on its screen, or by taking over.
- **Every request to the front is capped at 1 MiB** (`glass_http::MAX_BODY`).
  The desktop app passes its calls through the same path.
- **Labels (ADR 0018) decide only where data may go to a model.**
  - The four labels are Public, Internal, Confidential and Restricted.
  - They come from folder rules and from a label stored on the file.
  - A task's label rises with what it reads (DP-11).
  - Nothing checks a file's label when it leaves by any other way.
- **What IT already controls:**
  - settings per group, in `[group.settings]` as `{ value, locked }`;
  - USB drives on or off (ADR 0031).

  There is no rule on file types. Mail's attachment limits are fixed in
  code (10 files, 20 MB).
- **IT has nowhere to set these on Glasshouse Agents yet.** The admin
  console isn't in the AWS stack, so hosts use the image's default
  policy. ADR 0065 §9 plans the console there.
- **Others:** Azure Virtual Desktop and Windows 365 let IT allow or block
  the clipboard in each direction, and limit it to text or text and
  images, through Intune.
- **ADR 0073:** a person's files live in their home, kept on any host
  that holds their lease. Agents read it as the person, through the
  folders policy gives them.

## Decision
1. **The ways files move.**
   - **In:**
     - dropping a file on an agent or on its computer in the desktop app;
     - uploading in the web app;
     - `glass cp report.xlsx reconciler:`;
     - attaching files to an ask (`Ask.files`, now used).
   - **Out:**
     - a file an agent made comes back as a card to download (the "Done"
       card's changes become links);
     - downloading from the computer's files in the web app;
     - `glass cp reconciler:Reports/q3.pdf .`
   - **The clipboard while taking over:** text, and images where IT
     allows, under the same rules (CF-72).
2. **Where they land.**
   - In the person's home, in `~/Received`.
   - Agents read it as policy allows (`agents.folders` includes it by
     default).
   - Anything in the person's home may go out, if the rules below allow.
3. **They go around the 1 MiB cap, never through it.**
   - **Upload:** the front gives a one-time upload address. In AWS that
     is a presigned S3 `PUT` into the stack's staging bucket, encrypted
     with the org's KMS key and short-lived. glassd fetches the file,
     checks it, and only then places it.
   - **Download:** glassd checks the file, puts it in staging, and the
     front gives a one-time presigned `GET`, valid for minutes.
   - **Development:** a chunked route on the front does the same.
   - The checks are glassd's, the broker's. The person's device and the
     front are never trusted to have checked.
4. **The checks, in order.** A refusal is said in words the person can
   act on, such as "Spreadsheets and PDFs only: this is a program."
   - **The direction:** in or out, on for the person's group.
   - **The size:** at most the group's limit.
   - **What it is,** read from its content (its magic bytes), not its
     name.
     - Each kind IT can allow is a named group of types: documents,
       spreadsheets, presentations, PDFs, images, text and CSV, archives,
       code, programs.
     - A file whose content and name disagree is refused, such as a
       program named `.pdf`.
     - An archive is opened, and each file in it is checked the same way,
       to a fixed depth.
     - An encrypted archive can't be checked, so it is refused unless IT
       allows them.
   - **Its label, going out** (ADR 0018). The label is the file's own,
     its folder's rule, or what the task that wrote it read.
     - Up to the group's level (Internal by default), it goes.
     - Above that, Confidential asks first: an approval to the person, or
       to approvers IT names.
     - Restricted never leaves.
   - **Scanned, coming in,** where IT turns it on (EP-3).
     - In AWS, GuardDuty Malware Protection for S3 scans the staging
       bucket and tags each object with what it found.
     - glassd places nothing until a scan comes back clean.
   - **Its name:**
     - made safe: no path, no `..`, no control characters, no leading dot;
     - never executable once placed;
     - owned by the person.
5. **IT's settings, per group** (`Scope::Person`):
   ```toml
   [[group]]
   name = "finance"
   [group.settings]
   "files.in"        = { value = true, locked = true }
   "files.out"       = { value = true, locked = true }
   "files.in_kinds"  = { value = ["documents", "spreadsheets", "pdf", "images", "text"], locked = true }
   "files.out_kinds" = { value = ["documents", "spreadsheets", "pdf"], locked = true }
   "files.max_mb"    = { value = 100, locked = true }
   "files.out_label" = { value = "internal", locked = true }
   "files.scan"      = { value = true, locked = true }
   "clipboard.in"    = { value = "text", locked = true }
   "clipboard.out"   = { value = "off", locked = true }
   ```
   - **The org's defaults:**
     - in and out on;
     - documents, spreadsheets, presentations, PDFs, images, text and
       archives allowed, but not programs or scripts;
     - 100 MB;
     - out up to Internal without asking;
     - scanning on where the stack has it;
     - the clipboard text-only both ways.
   - IT tightens or loosens it per group, as for every setting. The most
     restrictive of org, group and agent wins, as for `max_label`.
6. **On the record.** Every transfer, allowed or refused, is recorded
   (DW-21):
   - who;
   - which way;
   - from where: web, desktop app or CLI;
   - its name, size and SHA-256;
   - its kind and label;
   - the decision, and why.

   The content isn't in the record. The admin console lists transfers per
   person and group.
7. **Undo.**
   - A file put in is a change to the person's home, snapshotted first,
     so undo removes it.
   - A file taken out can't be recalled, as with a sent mail. That's why
     out is the stricter direction.

## Prerequisites
- **The admin console in the AWS stack** (ADR 0065 §9). Until then,
  these rules live in the stack's policy file, set at deploy.
- **ADR 0073's homes,** for where files land on pooled hosts. On today's
  computers, files land in the computer's own `~/Received` the same way.

## Containment, when it's built
This puts files from outside into the person's home, and lets files out
of it. Before it ships, it goes through containment-review, with tests
and probe checks:
- a file never lands outside `~/Received`, whatever its name;
- nothing placed is executable;
- the checks run in glassd, after the upload, never on the device;
- a Restricted file can't leave by any way: a download, `glass cp`, an
  agent's card or the clipboard;
- every refusal is on the record.

## Slices
1. In and out through the web app and `glass cp`, with:
   - the chunked route (development);
   - the type, size and direction checks;
   - the settings and the record.
2. Labels on the way out: asks and Restricted. The broker's `decide()`
   gets a "leaves" check.
3. The desktop app's drop. Agents' files as download cards.
4. AWS: presigned staging, and GuardDuty scanning.
5. The clipboard while taking over.
