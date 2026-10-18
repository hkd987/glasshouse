# Architecture decision records

One file per decision that is hard to reverse or would surprise a new
engineer. Status is Accepted unless noted. Supersede, don't edit: write a new
ADR and mark the old one superseded.

| # | Decision |
|---|---|
| [0001](0001-rust-workspace-layering.md) | One Rust workspace, layered lib → svc → bin |
| [0002](0002-freebsd-release-overlay.md) | Build on unmodified FreeBSD 15.1 releases plus our overlay; develop in a QEMU VM |
| [0003](0003-single-daemon-for-prototype.md) | One privileged daemon hosting services as libraries, for now |
| [0004](0004-mcp-over-task-sockets.md) | Agents speak MCP over a per-task Unix socket; JSON-RPC everywhere |
| [0005](0005-brokered-file-access.md) | Agents get no user data mounted; file access is brokered |
| [0006](0006-file-level-undo.md) | Undo restores files from snapshots, never rolls back datasets |
| [0007](0007-drive-freebsd-through-admin-commands.md) | Drive jails and ZFS through their admin commands |
| [0008](0008-agent-manifests-declare-privileges.md) | Proposed: agents ship signed manifests; effective rights = declaration ∩ policy ∩ scope |
| [0009](0009-native-shell-web-apps.md) | A native Rust shell on Smithay and iced; apps are TypeScript web apps |
| [0010](0010-agents-are-harness-bundles.md) | Proposed: agents are versioned harness bundles run by one runner; hard limits stay in the OS |
| [0011](0011-branding-on-a-freebsd-base.md) | Branding: overlay now (tested), our own base build next; notices and SBOM generated from the build |
| [0012](0012-system-actions-broker-role.md) | IT self-service runs as fixed-function system actions in glassd; policy is the broker role |
