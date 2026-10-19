# Architecture decision records

One file per decision that is hard to reverse or would surprise a new
engineer. Status is Accepted unless noted. Supersede, don't edit: write a new
ADR and mark the old one superseded.

| # | Decision |
|---|---|
| [0001](0001-rust-workspace-layering.md) | One Rust workspace, layered lib → svc → bin (apps layer added by 0013) |
| [0002](0002-freebsd-release-overlay.md) | Build on unmodified FreeBSD 15.1 releases plus our overlay; develop in a QEMU VM |
| [0003](0003-single-daemon-for-prototype.md) | One privileged daemon hosting services as libraries, for now |
| [0004](0004-mcp-over-task-sockets.md) | Agents speak MCP over a per-task Unix socket; JSON-RPC everywhere |
| [0005](0005-brokered-file-access.md) | Agents get no user data mounted; file access is brokered |
| [0006](0006-file-level-undo.md) | Undo restores files from snapshots, never rolls back datasets (superseded in part by 0017) |
| [0007](0007-drive-freebsd-through-admin-commands.md) | Drive jails and ZFS through their admin commands |
| [0008](0008-agent-manifests-declare-privileges.md) | Proposed: agents ship signed manifests; effective rights = declaration ∩ policy ∩ scope |
| [0009](0009-native-shell-web-apps.md) | A native Rust shell on Smithay and iced; apps are TypeScript web apps |
| [0010](0010-agents-are-harness-bundles.md) | Proposed: agents are versioned harness bundles run by one runner; hard limits stay in the OS |
| [0011](0011-branding-on-a-freebsd-base.md) | Branding: overlay now (tested), our own base build next; notices and SBOM generated from the build |
| [0012](0012-system-actions-broker-role.md) | IT self-service runs as fixed-function system actions in glassd; policy is the broker role |
| [0013](0013-one-crate-per-app.md) | One crate per app's actions, built against a lib-layer contract; layers lib → apps → svc → bin |
| [0014](0014-tailoring-layers.md) | Tailoring is a stack of personal layers glassd checks and applies; layers only narrow, every change is a version |
| [0015](0015-take-over-in-your-harness.md) | Take over on an ended actions agent continues the task in the user's harness (Claude Code through `glass-mcp`); attended agents |
| [0016](0016-showing-the-shell.md) | The shell shows on the console framebuffer in the VM (verified, with evdev input) and through DRM on hardware; one compositor, two outputs |
| [0017](0017-undo-from-change-records.md) | Undo restores only the files the task changed, from its change records; a file changed since is left and reported; take-over edits are changes |
| [0018](0018-model-gateway.md) | glassd brokers every model call: `model_complete` is an action routed by the labels the task has read, redacted, audited without prompt text; HTTPS on the base system's OpenSSL; secrets in a root-only file until the broker exists |
| [0019](0019-signing-in.md) | Accepted: a greeter on the screen signs people in through PAM and starts their desktop with `su -l` |
| [0020](0020-agents-that-may-run-elsewhere.md) | Proposed: agents may later run in the cloud with local hands; the task socket stays the only interface and task state stays data (FD-1 to FD-4) |
| [0021](0021-managed-settings-load-from-disk.md) | Managed settings never block boot: glassd starts from the policy on disk, checked once, falling back to the last good one; new settings are fetched in the background and applied whole (fetch and live apply proposed) |
| [0022](0022-build-on-pkgbase.md) | Accepted: build on pkgbase; an image is a list of packages from one repository, ours included; devices take whole images; FreeBSD stays behind `glass-platform` |
| [0023](0023-admin-console-and-signed-sync.md) | Accepted: the admin console is its own workspace (`console/`); policy and catalog releases are signed with the org's ed25519 key; devices verify, validate whole and apply them in the background, keep working offline, and report; `catalog.search` scopes by the user's groups |
| [0024](0024-app-bridge.md) | An app is a principal its manifest bounds: glassd's app socket (uid-authorized, app methods only) runs each call through the agent pipeline, "by [app] for [user]"; undeclared actions are refused and audited; a user-level bridge serves the page (loopback dev mode only, for now). 0023 is on the admin console's branch |
| [0025](0025-native-first-party-apps.md) | Terminal and Activity Monitor are native apps (UI-6) on `glass-window`: iced views in Wayland windows, keys through the compositor's keymap; other first-party apps stay web apps |
