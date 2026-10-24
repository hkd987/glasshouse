# 0019. Signing in: a greeter on the screen, PAM, and `su` for the session

Status: **Accepted** (built on the dev VM; see "What we built"). PRD
UI-4, DK-1, CF-1 (later). Builds on ADR 0016.

## Context
The VM now shows the desktop on its own screen with live tiles, keyboard
and pointer (ADR 0016), but only because a developer hands the terminal
and input devices to `demo` and starts the shell over SSH. A machine has to
boot to a sign-in screen (the Sign in artboard), check the person's
password, and start their desktop as them, with the screen and input
theirs for the session and taken back when it ends.

On FreeBSD a display manager runs as root, authenticates through PAM and
starts the session with the user's credentials. `login(1)` and `su(1)`
already do the account and session part properly: groups, login class,
environment, `setlogin`, PAM session modules, lastlog.

## Decision
- **`glass-greeter`**, a binary in the shell workspace, runs as root on the
  first virtual terminal, started by `rc.d/glasshouse_greeter` at boot;
  `getty` no longer runs on `ttyv0` (the other terminals keep theirs, for
  administrators and for the branding checks).
- It draws the sign-in screen (`glass_shell::screens::login`) on the
  framebuffer and takes the keyboard and pointer from evdev, the same
  plumbing as the desktop (`glass-shell-fb`, which becomes a library plus
  two binaries so they share it).
- It lists the people who can sign in: local accounts with a uid of 1000 or
  more and a login shell. Directory sign-in (Entra, Okta: CF-1) replaces
  the list later.
- **Passwords go to PAM** (`glass_platform::auth`, OpenPAM, the
  `glasshouse` service in `/usr/local/etc/pam.d`, which includes `system`).
  A wrong password and an unknown account read the same. After a refusal
  the screen waits two seconds before taking another try. The password is
  wiped from memory after the check.
- **The session is `su -l <user> -c /usr/local/bin/glass-shell-fb`**, run
  by the greeter (root, so `su` asks for nothing more) after the check
  passes, so the account and session setup is the base system's own
  (ADR 0007). Before starting it the greeter closes the framebuffer and
  input devices and gives `ttyv0` and `/dev/input/event*` to the user;
  when the session ends it takes them back (root, mode 0600) and shows the
  sign-in screen again.
- Development VMs give `demo` a known password (`demo`) when provisioned;
  image builds never set one.

## Consequences
- The machine boots into Glasshouse's own UI; the text console stays on the
  other virtual terminals.
- The greeter is root code that reads untrusted keystrokes. It keeps no
  state between attempts beyond the delay, links PAM only through the
  `pam` feature of `glass-platform`, and never talks to glassd.
- Locking the screen, switching users and fast user switching need the
  compositor (ADR 0016's next step), which will own the devices for every
  session; until then one person is signed in at a time.
- Passkeys and fingerprints (CF-2) are PAM modules too, so they fit here
  later. Until then the screen's hint asks only for the password.

## What we built
- `glass-greeter` is a supervisor loop plus the screen as a child process
  of itself (`--screen`): the child shows the sign-in screen, checks the
  password and prints `signed-in <user>`, and exiting closes everything it
  had open before the person's desktop opens the same devices.
- `glass-shell-fb` became a library (`display`: framebuffer, damage-tracked
  drawing, pointer, input) and the desktop binary; the greeter uses the
  library. Shift Super Q, or Sign out in the system menu, ends the session.
- **Restart and Shut Down** (the mark's menu, each asked first) need root,
  which the desktop doesn't have and the sign-in screen does: the desktop
  draws "Restarting…" and ends its session with an exit status of its own
  (`glass_shell::session`, 75 or 76), and the supervisor runs
  `shutdown -r now` or `shutdown -p now`. Only the person at the screen can
  ask, through the session the supervisor started. The base system's
  shutdown messages go to the first terminal, so the supervisor shows the
  last frame on `ttyv8` (no login there) and switches to it first; then the
  kernel's own shutdown splash. Check for Updates is in the menu, dimmed
  until updates ship as whole images (BS-4, EP-1). Neither "Restarting…"
  frame shows the pointer.
- **Lock Screen** (the mark's menu, paired with Sign out as on a Mac; Ctrl
  Super Q or Super L) covers the desktop with the sign-in screen's look for
  the person signed in; agents keep working, and no shortcut, click or
  banner gets through. Unlocking needs PAM, so the supervisor checks it: su
  runs the desktop with its standard input and output piped to the
  supervisor; the desktop writes `unlock`, a tab and the password as one
  line, and the supervisor checks it for the session's own account (never
  another), one request at a time, waits two seconds after a wrong one, and
  answers `unlocked` or `refused` on the desktop's input. The password is
  wiped once checked and never logged; other lines the desktop prints go to
  the log. Only a desktop the sign-in screen started can be locked (`Live::
  lockable`); one started by hand shows Lock Screen dimmed. The pipe is the
  desktop's own: agents run in jails under their own uids and never inherit
  it, and anything else that could write to it is the person's own process,
  which gains nothing it couldn't get from `su` (and waits as long).
- Handing over, the screen that ends leaves its last frame up and the
  terminal in graphics mode (`Framebuffer::hand_over`) for the next to draw
  over, so the text console never shows between the sign-in screen and the
  desktop, either way. Any other ending puts the console back.
- Stopping the service (`service glasshouse_greeter stop`, or a shutdown)
  sends SIGTERM to the supervisor, which ends whatever is showing (SIGTERM
  to the screen; to the desktop's process group, never to `su`, so `su`
  still closes the PAM session), waits for it, takes the devices back and
  exits. The screen and the desktop each put the terminal back to text on
  SIGTERM, so the console login reappears.
- `rc.d/glasshouse_greeter` runs it under `daemon(8)` and turns `getty` on
  `ttyv0` off at start (`/etc/ttys`, then `init` rereads it). It turns it
  back on only when the service is disabled: a getty started during a
  restart drew its login prompt over the new sign-in screen.
- Images ship both binaries (`dist.manifest`; their crates and fonts are in
  the notices) and `post-install.sh` turns the service on, so every deploy
  and image boots into the sign-in screen.
- Checked on the dev VM: it boots into the sign-in screen; a wrong password
  is refused after two seconds; `demo` signs in to their desktop with live
  tiles; signing out returns to the sign-in screen with the devices back
  with root; restarting or stopping the service leaves nothing behind.
  Locking from the menu or Ctrl Super Q shows the lock screen, a wrong
  password is refused, shortcuts do nothing while locked, and `demo`'s
  password brings the desktop back as it was.

## Amended by ADR 0053
- First-run setup writes down what it's making (`setup.making.toml`)
  before it makes anything; the sign-in screen's next start finishes an
  interrupted setup or undoes it, and `setup.toml` is written whole.
- The sign-in screen tells glassd who signed in until glassd hears, every
  2 s, and stops when the session ends (`specs/tla/Boot.tla`).
