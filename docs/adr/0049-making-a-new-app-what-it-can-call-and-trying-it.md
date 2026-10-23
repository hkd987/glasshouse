# 0049. Making a new app: what it can call, and trying it

Status: **Accepted, built** (2026-10-23). The owner: "We should also test
building a net new app for our OS and making sure the harness can handle
that in a simple way and it has the right tools." PRD GA-1, GA-2, GA-12,
AE-3. Builds on ADR 0032 (remix and share), ADR 0024 (apps and the
bridge), ADR 0044 (the script runner's engine).

## Context
The desktop agent on Claude Haiku 5.5 was asked, on the development host
with the studio's actions (`apps.*`), for "a new app called Today": today's
meetings, how many emails are in the inbox, and a button per meeting that
drafts a "running 5 minutes late" email to its attendees.

1. **Refused for nothing.** `apps.create` said Calendar, Routines,
   Settings, Artifacts and Chat "already meet this need". One word in
   common was a match: Settings "shows", Chat keeps "drafts", Routines
   "how" and "each". The person had said "not a change to Calendar or
   Mail", and the agent didn't read that as turning them down. It asked
   the person instead. $0.0153, nothing made.
2. **Built on a guess.** Next time it made the app, which opened with
   meetings and the inbox count. But no meeting had attendees:
   `calendar.read` returns `{ event: { attendees } }`, and the app read
   `attendees` from the top. The button could never draft. The agent can
   read an action's result when it calls the action itself, but it didn't,
   and an app may call actions its maker's agent can't.
3. **Built blind.** With the shapes in hand it made the app again, and it
   crashed on opening: `ReferenceError: bridge is not defined`. It had
   declared a global instead of importing the SDK. esbuild bundles that
   happily, and nothing ran the app before the person would have. The
   agent can't open a page.
4. **Mail keeps no read state**, so there is no unread count to show. The
   agent saw that from Mail's data and showed the inbox's size instead.

## Decision
1. **What matches a need is two of its words.** An app blocks a new one
   in `apps.create` when it shares two of the need's words, or its only
   one. Function words ("how", "each", "with", "new", "own", "show",
   "button") say nothing about a need, and a word repeated counts once,
   in `catalog.search` too. `declined` says that "a new app, not a change
   to Calendar" turns Calendar down, and so does the refusal.
2. **Every action may say what its result carries.**
   `ActionProvider::returns(action)` gives the shape of its `data`, as a
   small JSON Schema (`glass_actions::shape`: type, properties, required,
   additionalProperties, items, enum, description).
   - Declared now for Calendar, Mail (but its password flow), People and
     Files.
   - Each provider's test runs every action it declares and checks the
     real result against its shape (`shape::conforms`). That caught Mail's
     sender address being null on a computer that doesn't know it.
3. **`apps.actions`: what an app can do** (Auto, a read of the registry
   and policy):
   - Without names, every action an app the person makes may call, with
     how it runs there (without asking, telling them, asking first). The
     closest to a `need` first. The person's own actions, and anything
     their policy makes Never in such apps, are left out.
   - With names (12 at most), each one's input and its result's data as
     the TypeScript the app reads, field descriptions as comments, and
     `const result = await bridge.call("…", input)`.
   - `apps.create`, `apps.save` and the template say to read it before
     writing an app's calls.
4. **Every save is tried** (`glass-app-try`):
   - After the bundle is built, the studio runs it once in a make-believe
     page: as much of a browser as the SDK and apps use, with timers on a
     make-believe clock, en-US `Intl`, and a bridge that answers each
     declared action with sample data shaped as its result and keeps
     every call.
   - It opens the app, lets three make-believe seconds pass, presses each
     kind of button once, and reports what the page showed, as text with
     buttons in brackets, what it called with what input, and what was
     thrown or rejected and not caught.
   - Buttons with the same words are one kind, and so are buttons with the
     same code behind them: a list's rows (Mail's folders, its messages),
     not a toolbar's buttons, which each run their own. Mail gets three
     presses, not eight.
   - The save's words carry it: "Opening it showed: …", "It called …",
     "Pressing [Draft 'running late'] called mail.draft {…}, then it showed
     …", and "Problems to fix, then save again". The problems also cover
     calls to actions the manifest doesn't declare and inputs an action
     won't take (`shape::fits`: required fields, unknown fields, types,
     enums).
   - The version is kept either way, with its problems among its notes:
     the try-out is a stand-in for a browser, and a refusal from a gap in
     it would block an app that works.
5. **Trying runs where bundling does.**
   - `glass-app-try` is boa (ADR 0044's engine) and the page, statically
     linked (`make freebsd` builds it apart, `target/static`), alone in
     `/usr/local/lib/glasshouse/app-try`.
   - It runs in a jail of its own, made as the bundler's is: the build
     folder is its whole filesystem, no network, as nobody, with the task
     limits, for 20 s at most.
   - It gets the bundle and sample data made from shapes. No person's data
     ever reaches it, and nothing it calls happens.
   - A computer without it (`[studio] tryout`) saves without trying.

## Consequences
- **Live on Claude Haiku 5.5** (2026-10-23), the same request:
  - The agent read `apps.actions`, the bases and the template, and made
    Today in five calls, $0.0072.
  - The save told it what the app showed with sample data and that a draft
    was saved; it said so, and that it had only tried it with sample data.
  - Opened on the person's own data, Today listed their three meetings
    with their attendees and the inbox's seven messages. Its button saved
    a real draft to all three of the standup's attendees.
- Before: three tries for $0.03, one refused and two broken (no attendees,
  then a crash on opening).
- The try-out opens our own Mail, Calendar and Store on sample data. Mail
  draws its folders and messages and every folder button lists that
  folder. Routines and Settings draw "TypeError" where their actions
  declare no shape, which the save says ("… has no description of its
  data yet, so the try gave it none").
- Adding an action now means declaring what it returns (the `add-action`
  skill).
- Saving takes longer. On the emulated VM, Mail's remix saved in 2.0 s
  before and takes 7.4 s now on the first save after a deploy, about 6 s
  after (budget 10 s). The try itself takes 0.15 s natively and 3–5 s
  emulated, slowest the first time a newly installed program runs. Two
  things got it there: pressing one of a list's buttons, not each, and
  building the engine at opt-level 3 in the VM's profile too.
- Tests: `glass-actions` `ga1_…` (shapes, samples, inputs); the four
  providers' `ga1_what_each_action_returns_is_what_it_declares`;
  `glass-app-try` (a working page, what goes wrong and where, a page that
  never stops, and the SDK's template bundled by the real esbuild);
  `glass-app-studio` `ga1_…` and the refusal's `ga12_…`; runtime
  `studio::ga1_an_agent_making_an_app_reads_…`; the VM's `studio`
  scenario tries the remix of Mail in the jail.

## Not built yet
- Shapes for the other actions: Chat, Docs, Artifacts, Meet, Routines,
  Settings, the catalog.
- Typing into the app's fields when it's tried; only buttons are pressed.
- Mail's read and unread state (an inbox's unread count).
- The try-out's page is a stand-in: layout, CSS and some browser APIs
  aren't there, and a gap shows as a problem the agent may not be able to
  fix.
