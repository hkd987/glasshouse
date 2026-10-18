#!/usr/bin/env bash
# Runs the end-to-end suite in the dev VM. Deploy first (`make vm-deploy`).
#
#   1. single-user scenarios as `demo` (containment, delegation, labels, ...,
#      and `perf`, the latency budgets in docs/dev/performance.md)
#   2. isolation: a second user can't see, stop or answer demo's task, and
#   3. sleep: that task pauses before the machine sleeps and resumes after
#   4. crash recovery: kill -9 glassd mid-task; on restart it removes the
#      orphaned jail and the audit log is intact
#   5. the model gateway with the scripted provider, under a test policy
#   6. branding: no base-system name where a user looks (BR-1, BR-2)
#   7. root checks: audit chain verifies, no jails or mounts left behind
set -euo pipefail

repo=$(cd "$(dirname "$0")/../.." && pwd)
vm="$repo/os/freebsd/scripts/vm.sh"
profile="${FREEBSD_PROFILE:-vm}"
target="${CARGO_TARGET_DIR:-$repo/target}/x86_64-unknown-freebsd/$profile"
e2e=/usr/local/libexec/glasshouse-tests/glass-e2e

# Each step says how long the one before it took, so slow steps show up.
last=$SECONDS
step() {
	printf '   (%ss)\n\n== %s\n' "$((SECONDS - last))" "$*"
	last=$SECONDS
}
# Plain su sets HOME, USER and SHELL for the target user (su(1)) without
# running login scripts.
glass_jails='jls name | grep -c "^glass-" || true'

cargo build --profile "$profile" --target x86_64-unknown-freebsd -p glass-e2e
"$vm" run "mkdir -p ${e2e%/*}"
"$vm" scp-in "$target/glass-e2e" "$e2e"

step "scenarios as demo"
"$vm" run "su demo -c '$e2e all'"

step "isolation between users, and agents pause for sleep (PM-5)"
# One waiting task serves both: another user can't see it, and it pauses
# through the rc.suspend/rc.resume hook and carries on after.
task=$("$vm" run "su demo -c '$e2e hold'" | tail -1)
"$vm" run "pw usershow e2e-other >/dev/null 2>&1 || pw useradd e2e-other -m -s /bin/sh
su e2e-other -c '$e2e isolation $task'
service glassd suspend
su demo -c '$e2e sleeping $task'
service glassd resume
su demo -c '$e2e awake $task'
su demo -c '$e2e release $task'"

step "crash recovery"
task=$("$vm" run "su demo -c '$e2e hold'" | tail -1)
before=$("$vm" run "kill -9 \$(cat /var/run/glassd.pid); sleep 2; $glass_jails")
[[ $before -ge 1 ]] || { echo "FAIL expected an orphaned jail after kill -9, found $before"; exit 1; }
# The killed daemon's socket file is still there, so wait until the new one
# answers: it removes orphans before it listens.
"$vm" run "service glassd start >/tmp/glassd-start.log 2>&1
for i in \$(jot 120); do glassctl status >/dev/null 2>&1 && break; sleep 0.5; done"
after=$("$vm" run "$glass_jails")
[[ $after -eq 0 ]] || { echo "FAIL $after orphaned jails survived the restart"; exit 1; }
"$vm" run "grep -q 'removed a sandbox left behind' /var/log/glassd.log"
echo "PASS crash recovery (orphaned jail removed on restart)"

step "model gateway, scripted provider (DP-1, DP-2, DP-3, DP-5)"
# The scenario needs a model in policy: install the test policy (the default
# plus a scripted model the attended claude-code entry may call), restart
# glassd, run it, then put back whatever policy was there.
policy=/usr/local/etc/glasshouse/policy.toml
"$vm" run "set -e
if [ -e $policy ]; then mv $policy $policy.e2e-saved; fi
$e2e models-policy >$policy
service glassd restart </dev/null >/tmp/glassd-models.log 2>&1
for i in \$(jot 120); do glassctl status >/dev/null 2>&1 && break; sleep 0.5; done
status=0
su demo -c '$e2e models' || status=\$?
rm -f $policy
if [ -e $policy.e2e-saved ]; then mv $policy.e2e-saved $policy; fi
service glassd restart </dev/null >>/tmp/glassd-models.log 2>&1
for i in \$(jot 120); do glassctl status >/dev/null 2>&1 && break; sleep 0.5; done
exit \$status"

step "branding (BR-1, BR-2)"
"$vm" scp-in "$repo/tests/e2e/check-branding.sh" /tmp/check-branding.sh
"$vm" run "sh /tmp/check-branding.sh"

step "root checks"
# `hold` leaves its workspace for the task it starts; the tasks are gone now.
"$vm" run "rm -rf /home/demo/e2e-hold-*"
"$vm" run "glassctl verify
test \$($glass_jails) -eq 0 || { echo 'FAIL jails left behind'; exit 1; }
if mount -p | grep -q /var/db/glasshouse/sandboxes; then echo 'FAIL mounts left behind'; exit 1; fi
echo 'PASS nothing left behind'"

step "all end-to-end checks passed in ${SECONDS}s"
