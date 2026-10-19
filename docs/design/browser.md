# The Browser

The Browser should be fast, light on memory and calm to use: it's where
most of a person's day goes (PRD: Microsoft 365, Google Workspace and SaaS
run in it, CF-15). Until our own build (CF-15), it is the browser engine's
own browser (ADR 0027) with our frame around it, so what we can change is
what policies, flags and the desktop around it allow.

## Now (engine policies and the desktop)

| What | How |
|---|---|
| Opens in well under a second on hardware | The engine runs from sign-in with no window; the Browser's window is handed to it (ADR 0027 decision 3, `docs/dev/performance.md`) |
| One frame, ours | Server-side decorations; the tabs sit under our title bar (ADR 0027 decision 5) |
| Memory | Memory Saver on, balanced (`HighEfficiencyModeEnabled`, `MemorySaverModeSavings`): tabs not looked at for a while give their memory back and reload when chosen. One set of engine processes for the Browser and every web app |
| Picks up where you left off | Tabs come back at the next sign-in (`RestoreOnStartup`) |
| Privacy | Third-party cookies blocked, HTTPS upgrades on; no sign-in to the engine maker's account, no sync, metrics, promotions, shopping, Labs or translation (which needs the engine maker's servers) |
| Everyday defaults | Spelling checked; downloads go to `~/Downloads` without asking |
| BR-1 | Pages that name the base system blocked (`chrome://version`, `gpu`, `system`, `sandbox`) |

Mandatory policies are in `policies/managed/glasshouse.json`; the person can
change the recommended ones (`policies/recommended/glasshouse.json`) in
the Browser's settings. Both are under `/usr/local/etc/chromium/` in the
image overlay; every name was checked on the engine's own policy page
(none in error or deprecated). The console sets them per org later (CF-15).

## Waiting on a decision

- The start page and search engine (ADR 0027 question 3): the engine maker's
  new tab page and search, as now, or ours.

## With our own build (CF-15), in order

1. **Our name and start page.** No "Chromium" or "Customize Chromium";
   a start page with the person's recent files, mail and agents' work.
2. **Our tab strip and toolbar** in the desktop's style (UX-10 theme),
   compact, with the controls the desktop already draws left out.
3. **The command bar knows the Browser.** Ctrl Space finds open tabs,
   history and bookmarks next to apps and agents (CF-70).
4. **Agents work in it through actions** (NV-3: browser actions), with
   tiers, audit and undo, instead of driving the screen.
5. **Content blocking built in**, with our own filter engine (the popular
   blockers are GPL-3.0, which ADR 0027 rules out).
6. **Memory you can see.** Each tab's memory in the tab's hover card and
   in Activity Monitor; tabs asleep after a while by default.
7. **IT-managed extensions and policies from the console** (CF-15), and a
   compatibility suite (CF-17).
8. **GPU drawing** once the compositor drives the display directly
   (ADR 0016).
