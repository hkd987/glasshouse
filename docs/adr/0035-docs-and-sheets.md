# 0035. Docs and Sheets: shared items edited live as CRDTs

Status: **Accepted** (2026-10-20). PRD DC-1 to DC-6 and SH-1 to SH-3,
under the owner's decision that changed NV-3. Builds on ADR 0033 (the
shared space), 0034 (item labels), 0030 (chat) and 0024 (the app bridge).

## Context
- People want a clean, Notion-like doc editor and an Excel-like sheet,
  both with AI built in, shared with colleagues and edited together.
- ADR 0033 §8 already chose the outline:
  - docs and sheets are shared-space items;
  - edits are CRDT updates (Yjs, MIT) sealed as messages;
  - snapshots are sealed blobs;
  - the console relays what it can't read, and pages merge.
- Four facts in the code shape the rest:
  - **Every app call is an action, audited in the hash-chained log.** A
    page sending an update per keystroke burst would write a few records
    a second per editor.
  - **Chat keeps one JSON store per person.** It is rewritten on every
    message, and opening an item decrypts all of its messages. A doc with
    hundreds of updates would slow every chat post and every open.
  - **Undo restores files from snapshots.** Nothing lets a provider undo
    its own change, so an edit that reaches other people can't honestly
    be called reversible yet.
  - **App sessions have no models,** and the bridge's CSP is chosen by
    app id.
- Agents must read docs and sheets as text and values (DC-6). So the
  computer has to understand the CRDT, not just relay it.

## Decision
1. **Kinds and content.** `doc` and `sheet` are shared-space item kinds
   with labels (ADR 0034 §5). Each item's content is one Yjs document:
   - in pages, Yjs (MIT);
   - on the computer, yrs (MIT), Yjs's Rust port, which reads and writes
     the same encoding.

   A doc is a ProseMirror document in the Yjs `XmlFragment` named
   `prosemirror`, through y-prosemirror (MIT). A sheet is described in §8.
2. **Parts.**
   - `Update { data }`: a Yjs v1 update, base64, of at most 8 KB raw.
     Larger ones go as `UpdateBlob { blob, key, sha256, bytes }`.
   - `Snapshot { blob, key, sha256, bytes, upto, name }`: the whole state.
     Readers start from the newest snapshot and apply every later update;
     Yjs merges duplicates.
     - An editor's computer posts one when the log since the last passes
       200 updates or 512 KB.
     - A named snapshot is a version in the history (DC-3).
   - `Roles { editors, commenters }`, from the owner. Everyone else in the
     item views. Computers drop updates from anyone who isn't an editor.
     Membership is fixed when the item is made, until chat C2; roles
     within it change at any time.
   - Comments, replies and resolves reuse artifacts' parts. A comment's
     anchor path is a Yjs relative position, base64.
3. **Live editing is a session, not a stream of actions.**
   - `doc.open` (and `sheet.open`) is an audited action. It returns:
     - the merged state as one update;
     - a cursor;
     - the person's role.
   - While that app session stays open, its page pushes and pulls updates
     for that item over a new app-socket method, `app.live`
     (`/_bridge/live`):
     - push seals and posts;
     - pull returns the updates after a cursor, waiting up to 25 s for
       new ones, like `app.status`.
   - `app.live` is not an action. It is allowed only for an item the
     session opened, only in the role it opened with. Closing writes one
     audit record: "edited Q3 plan: 14 minutes, 340 changes".
   - Peers hear of a change through chat's sync, which wakes waiting
     pulls at once.
4. **Items keep their own logs on the computer.** Messages of item kinds
   go to an append-only file per item beside the person's chat store, not
   into `store.json`.
   - `Shared::since(login, id, seq)` opens only what's new.
   - Once a snapshot lands, this computer drops the updates it covers. The
     console keeps them, for retention and holds.
   - glassd keeps each open item's merged yrs state in memory, so a
     reopen costs nothing.
   - Opening a doc of 10,000 updates must stay under DC-1's second. That
     budget is in `docs/dev/performance.md` from the start.
5. **Agents** (crate `glass-app-docs`):

   | Action | Tier | Notes |
   |---|---|---|
   | `doc.list`, `sheet.list` | Auto | |
   | `doc.read` | Auto | Markdown; raises the label |
   | `sheet.read` | Auto | Values and formulas of a range; raises the label |
   | `doc.create`, `sheet.create` | Ask | With people, Irreversible |
   | `doc.write` | Ask, Notify once undo can revert it (§6) | Markdown appended, replacing, or after a heading |
   | `sheet.write` | Ask, Notify once undo can revert it (§6) | Cells: values or formulas |
   | `doc.comment` | Ask | Others see it |

   - The Docs and Sheets apps' `[[app]]` tiers make the person's own
     create, write and comment Auto.
   - An agent's edit is a Yjs update made by yrs. It carries an `ai` mark
     naming the agent, so pages show it as the agent's (DC-2).
6. **Undo for edits that reach others.** `ActionProvider` gains an `undo`
   hook, which a change record names. For a doc or sheet edit, the hook
   posts the inverse update: yrs's UndoManager, scoped to that call's
   origin. Once it lands, `doc.write` and `sheet.write` become Notify,
   as DC-6 asks, and `glassctl undo` reverts them. Until then they ask.
   The PRD's Notify is kept as the target, not claimed.
7. **AI in the apps** (DC-2, SH-2).
   - A manifest may ask for the `models` permission. An app session that
     has it may call `model.complete`, routed by the session's data label.
     Opening an item raises that label to the item's.
   - The app shows what it sends.
   - Its answer is applied as an edit marked `ai` with the model's name,
     undoable in the page (the Yjs UndoManager) and kept in history.
   - A remix that adds `models` is a privilege delta in the store's review
     (ADR 0032).
   - Per-app CSP comes from the manifest's permissions, not the app's id,
     so remixes get what their manifest declares:
     - `frame-src` for artifact origins for an app that embeds artifacts;
     - `'wasm-unsafe-eval'` for an app that uses the formula engine.
8. **Sheets.**
   - A sheet's Yjs document holds `sheets`: per tab, a map of cells
     (`A1` → `{ f?: formula, v?: value, s?: style }`), plus row and column
     sizes.
   - Values are never stored for formulas. Everyone computes them with
     one engine, `glass-formula`, our own (the open engines are GPL):
     - natively in glassd, so `sheet.read` gives agents real values;
     - compiled to `wasm32-unknown-unknown` for the Sheets page, so both
       agree to the digit.
   - This is the one carve-out from "one target": the formula engine
     alone is built for wasm, for apps. The SessionStart hook and CI add
     the target, and `make apps-build` builds it.
   - The functions are the common Excel ones, listed in `glass-formula`'s
     docs. Errors are Excel's (`#DIV/0!`, `#REF!`, `#NAME?`, `#VALUE!`,
     `#N/A`, `#CYCLE!`).
9. **Mentions and email.** Typing a colleague's name fills in their login
   and email from the directory (AD-1). An @mention posts a `Mention`
   part, and the mentioned person's computer shows a notice (DC-4).

## Consequences
- What people write together stays sealed on our servers like chat, and
  is readable by agents through the same pipeline, labels and audit as
  files.
- Typing produces one audit record per session, not per keystroke. The
  record says how much changed, and the item's own history says what.
- Two new runtime surfaces: `app.live` and the provider undo hook. Both
  get containment review and pipeline tests.
- yrs and the wasm target join the build. yrs is pure Rust; its
  compile-time cost is measured before it lands
  (`docs/dev/build-performance.md`).

## Slices
- **D1:**
  - the doc kind and parts, with the item logs (§4);
  - `glass-app-docs` with yrs: list, read, create, write and comment;
    write asks;
  - `app.live` and its audit;
  - the Docs app:
    - headings, lists, checklists, tables, code, callouts, links between
      docs, embedded artifacts;
    - a slash menu;
    - live co-editing;
    - comments;
    - named versions;
    - sharing at creation, with people autocomplete and roles.

  Budgets: open under a second, and an edit on one computer visible on
  another.
- **D2:**
  - the `models` permission, with AI in Docs;
  - manifest-driven CSP;
  - mentions;
  - suggested edits;
  - presence and cursors;
  - the undo hook, with writes at Notify.
- **S1:**
  - the sheet kind;
  - `glass-formula`, native and wasm;
  - the grid: formulas, formatting, sort and filter;
  - CSV in and out;
  - `sheet.*` actions;
  - chunked blobs for big snapshots.
- **S2:** pivot tables, charts, Excel in and out, and AI in Sheets.
- **Later:**
  - Word in and PDF out (DC-5, P1);
  - membership changes after creation, which need chat C2;
  - IT's compliance view rendering docs and sheets.
