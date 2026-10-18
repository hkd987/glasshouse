#!/usr/bin/env bash
# Runs the end-to-end suite in the dev VM. Deploy first (`make vm-deploy`).
#
#   1. single-user scenarios as `demo` (containment, delegation, labels, ...)
#   2. isolation: a second user can't see, stop or answer demo's task
#   3. sleep: running agents pause before the machine sleeps and resume after
#   4. crash recovery: kill -9 glassd mid-task; on restart it removes the
#      orphaned jail and the audit log is intact
#   5. root checks: audit chain verifies, no jails or mounts left behind
set -euo pipefail

repo=$(cd "$(dirname "$0")/../.." && pwd)
vm="$repo/os/freebsd/scripts/vm.sh"
profile="${FREEBSD_PROFILE:-vm}"
target="${CARGO_TARGET_DIR:-$repo/target}/x86_64-unknown-freebsd/$profile"
e2e=/usr/local/libexec/glasshouse-tests/glass-e2e

step() { printf '\n== %s\n' "$*"; }
# Plain su sets HOME, USER and SHELL for the target user (su(1)) without
# running login scripts.
glass_jails='jls name | grep -c "^glass-" || true'

cargo build --profile "$profile" --target x86_64-unknown-freebsd -p glass-e2e
"$vm" run "mkdir -p ${e2e%/*}"
"$vm" scp-in "$target/glass-e2e" "$e2e"

step "scenarios as demo"
"$vm" run "su demo -c '$e2e all'"

step "isolation between users"
task=$("$vm" run "su demo -c '$e2e hold'" | tail -1)
"$vm" run "pw usershow e2e-other >/dev/null 2>&1 || pw useradd e2e-other -m -s /bin/sh
su e2e-other -c '$e2e isolation $task'"
"$vm" run "su demo -c '$e2e release $task'"

step "agents pause for sleep (PM-5), through the rc.suspend/rc.resume hook"
task=$("$vm" run "su demo -c '$e2e hold'" | tail -1)
"$vm" run "service glassd suspend"
"$vm" run "su demo -c '$e2e sleeping $task'"
"$vm" run "service glassd resume"
"$vm" run "su demo -c '$e2e awake $task'"
"$vm" run "su demo -c '$e2e release $task'"

step "crash recovery"
task=$("$vm" run "su demo -c '$e2e hold'" | tail -1)
before=$("$vm" run "kill -9 \$(cat /var/run/glassd.pid); sleep 2; $glass_jails")
[[ $before -ge 1 ]] || { echo "FAIL expected an orphaned jail after kill -9, found $before"; exit 1; }
"$vm" run "service glassd start >/tmp/glassd-start.log 2>&1
for i in \$(jot 60); do [ -S /var/run/glasshouse/control.sock ] && break; sleep 1; done"
after=$("$vm" run "$glass_jails")
[[ $after -eq 0 ]] || { echo "FAIL $after orphaned jails survived the restart"; exit 1; }
"$vm" run "grep -q 'removed a sandbox left behind' /var/log/glassd.log"
echo "PASS crash recovery (orphaned jail removed on restart)"

step "root checks"
"$vm" run "glassctl verify
test \$($glass_jails) -eq 0 || { echo 'FAIL jails left behind'; exit 1; }
if mount -p | grep -q /var/db/glasshouse/sandboxes; then echo 'FAIL mounts left behind'; exit 1; fi
echo 'PASS nothing left behind'"

step "all end-to-end checks passed"
