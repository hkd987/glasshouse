# 0050. Mail keeps what the person has read

Status: **Accepted, built** (2026-10-23). The owner, after ADR 0049's live
test: "Ok let's fix the missing items". One of them: an app the agent made
couldn't show how many emails are unread, because Mail kept no read state.
PRD EM-3 (phones and other mail apps work; Mail behaves the same on any
provider), IN-2, UI-5. Builds on ADR 0037 (mail through IMAP, `~/Mail` the
cache) and ADR 0049.

## Context
- `~/Mail` is a cache of the person's service, a file per message named
  after its UIDVALIDITY and UID. Without a service it's the whole store.
- The sync fetched with `BODY.PEEK` and never asked for flags, so nothing
  read anywhere showed here.
- Mail's folder buttons showed how many messages each folder had. Its
  code called that number "unread".
- Undo restores files from snapshots. A change on the server needs a
  provider's `revert` hook, as `mail.move` has.

## Decision
1. **A read marker per message.** The person has read a message when
   `~/Mail/<Folder>/.read/<file>` exists: an empty file, theirs, written
   through `ConfinedDir`.
   - Drafts and Sent keep no markers. They're the person's own, always read.
   - Moving or removing a message carries its marker along, so a filed
     message stays read (IMAP `MOVE` keeps its flags too).
   - A message with no marker is unread. Mail in the local store arrives
     unread.
2. **The server's `\Seen` flag is the truth where there is a server.**
   - Every sync asks `UID SEARCH UNSEEN`, even when nothing arrived (read
     state changes without UIDNEXT or EXISTS moving). Usually far fewer
     messages are unread than read.
   - The markers of the folder's synced messages are then set to match.
   - A sync that changed any wakes Mail, as new mail does.
3. **`mail.mark {ids, read}`: marked here and on the server.**
   - The server first (`UID STORE ±FLAGS.SILENT (\Seen)`), under the
     person's sync lock: a refusal changes nothing, and a sync can't put
     the old state back halfway.
   - Out of reach, or not connected, a mark is made here and waits in
     glassd's own sync state (root's, never the home), by UID under the
     folder's UIDVALIDITY. The next sync tells the server first. The
     result says so.
   - A change per message names its marker, so undo puts the marker back
     from the snapshot. `revert` then tells the server what the marker
     says now.
   - Notify for agents: marking mail changes what the person sees as new.
     Auto in Mail (its `[[app]]` tiers).
4. **Reading never marks.** `mail.read` leaves a message as it was, so an
   agent going through the inbox leaves what's new to the person new. Mail
   marks a message read when the person opens it.
5. **What results say.**
   - Each message carries `unread`, and `mail.list` carries `unread`: how
     many in the whole folder, listed or not.
   - Its text says "7 messages in Inbox, 2 unread".
6. **Mail shows it.**
   - Unread rows have a dot and a bold subject.
   - Folders show how many are unread, and Drafts how many drafts.
   - Opening a message marks it read; "Mark unread" puts it back and
     returns to the list.

## Consequences
- An app can show "3 unread": `mail.list`'s data says so, and
  `apps.actions` shows it to the agent making the app.
- Read state is shared with the person's phone through the server: read
  there, it's read here after the next sync, and the other way round.
- Each mark from Mail is an audited action, as Mail's moves are.
- Tests:
  - `glass-app-mail` `em3_…`: the server's state arrives, reading doesn't
    mark, a mark goes to the server, undo marks back, and a mark made
    offline waits for the next sync. Also: filed mail stays read; the
    local store; drafts are always read.
  - `glass-gateway-providers` `em3_read_state_is_the_servers_seen_flag_both_ways`,
    against a real server (mox): unread after a sync that brought nothing
    new.
  - The runtime's `em3_an_agents_mark_tells_the_person_and_undo_marks_it_back_on_the_server`
    on mox.
  - The `in2_…` comparison now marks too.
  - Mail's Playwright test `em3 opening a message marks it read, and Mark
    unread brings it back`.
- The test mail server's `deliver` now leaves mail unread. Filing a
  message marks it read, as Sent needs.

## Not built yet
- A count across folders for the launcher's tile.
- Flags other than `\Seen` (flagged, answered).
