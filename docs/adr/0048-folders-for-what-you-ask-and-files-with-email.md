# 0048. Folders for what you ask, and files that go with an email

Status: **Accepted, built** (2026-10-23). The owner's example in ADR 0045:
"hey x, grab abc report and send it to Susan". Its "Next, needed for the
owner's example": folders for tasks the person starts by voice or from
the command bar, and Mail attachments. PRD DA-1, AE-3, EM.

## Context
- The command bar and "Hey Glass" start the desktop agent with no
  folders. "Grab the ABC report" had nowhere to look.
- Mail sent text only: no message carried a file, and reading one said
  nothing of the files it carried.
- A task's folders are what it may read (and judge by label); anything
  that leaves the computer asks first (`mail.send`, Ask).

## Decision
1. **The person's folders for their agent.** A setting, `agents.folders`
   ("Folders your agent reads", Agents):
   - folders in their home, by name, Documents by default;
   - the person's to change; IT may set a default or lock it, as with any
     of theirs (ADR 0029). A value that isn't a folder in a home (`..`,
     `/etc`) is refused.
   - A request to the desktop agent that names no folders (the command
     bar's, "Hey Glass") reads these, read only.
   - Each is checked as a named one is (in the home, a folder, no link
     out of it); one that isn't there is left out.
   - A request that names folders, any other agent, and a routine's run
     (its saved scope) get exactly what they name.
2. **Files go with an email.**
   - `mail.draft` and `mail.send` take `attachments`: paths in the task's
     folders, as `files.list` gives them; at most 10 files, 20 MB.
   - Each is resolved in the preview, so policy and labels judge it as
     any read, and a path outside the task's folders is refused before
     anything is read.
   - The review's line names them ("To dana@…: ABC report · attached: ABC
     report.pdf"), with the body under it (ADR 0045's fix).
   - glassd reads them when it builds the message, as one multipart
     message.
   - The record names each file that left, its size and SHA-256, never
     its contents.
3. **A draft's files go with it.** `mail.send` may name the draft a
   message is (`draft: "drafts/…"`): what that draft carries goes too.
   - Mail's Send on a draft names it. Before, an agent's draft with a file
     would have gone without it: the app can't name files, having no
     folders.
   - Only a draft: a message in another folder is refused.
4. **Reading shows what a message carries.** `mail.read` lists each file
   by name, type and size, never its contents. Mail shows them under the
   message.
5. **The desktop agent knows.** Its instructions say the files the person
   mentions are in its folders (`files_list` with no path), and that a
   file goes with an email by its path in `attachments`.

## Consequences
- **Live on Claude Haiku 5.5** (2026-10-23), in a transcribed meeting:
  - "Hey Glass, grab the ABC report and send it to Dana" listed the
    agent's folders, found `ABC report.pdf` in Documents, looked Dana up
    and asked to send the email with it.
  - The approval showed the body and the file; approved, Dana's email
    carried the PDF. $0.0014, four model calls.
  - It said it hadn't opened the file to check it was the final version
    Priya asked for, and left her own "Hey Glass" alone.
- The desktop agent now reads the person's Documents unless they or IT
  choose otherwise. It changes nothing there, and sending still asks.
- Mail sent through IT's service carries the files as sent here.

## Not built yet
- Attaching a file in Mail itself: the app has no folders, so only an
  agent's files, or a draft's, go.
- Opening or saving a received message's file: Mail lists them.
- Scanning files that arrive (EP-3) before an agent may read them.
