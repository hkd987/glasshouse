#!/usr/bin/env bash
# Usage: tests/soak/soak.sh [--hours N] [--crash-every MIN] [--power-cut-every MIN] [--out DIR]
# A soak test in the dev VM (`make freebsd vm-deploy` first): agents working without a break
# for hours, as a person would have them, and what must stay bounded sampled as it runs.
#
# Each round, as demo: Invoice demo reconciles the invoices and asks to send reminders, which
# are approved; the IT agent clears the cache; every third round the invoice task is undone;
# every tenth, the finished tiles are dismissed. glassd is killed outright every
# --crash-every minutes (its supervisor starts it again, ADR 0055) and, with
# --power-cut-every, the VM loses power. Every round samples glassd's memory, open files and
# threads, the tasks it keeps, jails, sandboxes, snapshots, the audit log and the pool's free
# space into metrics.csv; every half hour the audit log is verified.
#
# Fails (exit 1) when: a round doesn't finish; the audit log doesn't verify; a jail or a
# sandbox is left with no task running; glassd's open files grow past 4x where they settled;
# or its memory past 3x, within one run of glassd. `failures` says which.
set -euo pipefail

repo=$(cd "$(dirname "$0")/../.." && pwd)
vm="$repo/os/freebsd/scripts/vm.sh"
hours=2 crash_every=30 cut_every=0
out="$repo/build/soak/$(date -u +%Y%m%dT%H%M%SZ)"
while (($#)); do
	case $1 in
	--hours) hours=$2; shift ;;
	--crash-every) crash_every=$2; shift ;;
	--power-cut-every) cut_every=$2; shift ;;
	--out) out=$2; shift ;;
	*) echo "usage: $0 [--hours N] [--crash-every MIN] [--power-cut-every MIN] [--out DIR]" >&2; exit 2 ;;
	esac
	shift
done
mkdir -p "$out"
log() { printf '%s %s\n' "$(date -u +%H:%M:%S)" "$*" | tee -a "$out/soak.log"; }
# A failure carries what the last command said on stderr, and its exit status (written to a
# file: most commands run in a subshell).
fail() {
	log "FAIL $* ($(tail -c 300 "$out/last.err" | tr '\n' ' '))"
	echo "FAIL $*" >>"$out/failures"
}
in_vm() {
	local status=0
	"$vm" ssh "$1" 2>"$out/last.err" || status=$?
	echo "exit $status" >>"$out/last.err"
	return "$status"
}
as_demo() { in_vm "su demo -c '$*'"; }
as_root() { in_vm "$*"; }
json_field() { python3 -c "import json,sys; d=json.load(sys.stdin); print($1)" 2>/dev/null; }
ready() { for _ in $(seq 120); do as_root 'glassctl status >/dev/null 2>&1' && return 0; sleep 1; done; return 1; }

# One sample: the round, glassd's pid, its memory (KB), open files and threads, the tasks it
# keeps for demo, jails, sandbox entries, snapshots, the audit log's size and the pool's free
# space (bytes).
echo "time,round,pid,rss_kb,fds,threads,tasks,jails,sandboxes,snapshots,audit_bytes,pool_free,round_s" >"$out/metrics.csv"
sample() {
	local round=$1 took=$2 row
	# shellcheck disable=SC2016 # Expanded in the VM.
	row=$(as_root 'p=$(cat /var/run/glassd.pid 2>/dev/null || echo 0)
		r=$(ps -o rss= -p $p 2>/dev/null | tr -d " "); f=$(procstat -f $p 2>/dev/null | wc -l | tr -d " ")
		t=$(procstat -t $p 2>/dev/null | wc -l | tr -d " ")
		j=$(jls name | grep -c "^glass-" || true); s=$(ls /var/db/glasshouse/sandboxes | wc -l | tr -d " ")
		z=$(zfs list -H -t snapshot | wc -l | tr -d " "); a=$(stat -f %z /var/db/glasshouse/audit/audit.log)
		free=$(zpool list -Hp -o free zroot)
		echo "$p,${r:-0},${f:-0},${t:-0},$j,$s,$z,$a,$free"')
	local tasks
	tasks=$(as_demo 'glassctl --json tasks' | json_field 'len(d)' || echo 0)
	IFS=, read -r pid rss fds threads jails sandboxes snapshots audit free <<<"$row"
	echo "$(date -u +%FT%TZ),$round,$pid,$rss,$fds,$threads,$tasks,$jails,$sandboxes,$snapshots,$audit,$free,$took" >>"$out/metrics.csv"
	# Nothing of a task is left once none is live (PA-2). A task that just ended may still be
	# taking its sandbox down, so look again before calling it left behind.
	local tries
	for tries in 1 2 3; do
		((jails > 0 || sandboxes > 0)) || break
		local states left
		states=$(as_demo 'glassctl --json tasks' | json_field '" ".join(t["id"][-8:] + ":" + t["state"] for t in d if t["state"] not in ("done","failed","stopped"))') ||
			{ fail "round $round: couldn't list tasks"; return; }
		[[ -n $states ]] && break
		if ((tries == 3)); then
			left=$(as_root 'jls name; ls /var/db/glasshouse/sandboxes' | tr '\n' ' ')
			fail "round $round: left with no task live: $left"
			break
		fi
		sleep 5
		# shellcheck disable=SC2016 # Expanded in the VM.
		IFS=, read -r jails sandboxes <<<"$(as_root 'echo "$(jls name | grep -c "^glass-" || true),$(ls /var/db/glasshouse/sandboxes | wc -l | tr -d " ")"')"
	done
}

verify() {
	local said
	said=$(as_root 'glassctl verify' || true)
	log "verify: $said"
	[[ $said == "Audit log intact"* ]] || fail "the audit log doesn't verify: $said"
}

round() {
	local n=$1 id ask
	id=$(as_demo 'glassctl --json run invoice-demo "Reconcile this month" --read ~/Invoices --write ~/Reports --yes' | json_field 'd["id"]') ||
		{ fail "round $n: invoice task didn't start"; return; }
	as_demo "glassctl wait --until needs-you --timeout 180 $id" >/dev/null ||
		{ fail "round $n: invoice task never asked"; return; }
	ask=$(as_demo 'glassctl --json approvals' | json_field "[a['id'] for a in d if a['task']=='$id'][0]") ||
		{ fail "round $n: no ask for $id"; return; }
	as_demo "glassctl approve $ask" >/dev/null
	as_demo "glassctl wait --timeout 180 $id" >/dev/null || fail "round $n: invoice task didn't end"
	if ((n % 3 == 0)); then
		as_demo "glassctl undo $id" >/dev/null || fail "round $n: undo of $id failed"
	fi
	id=$(as_demo 'glassctl --json run it-agent "My disk is filling up" --yes' | json_field 'd["id"]') ||
		{ fail "round $n: IT agent didn't start"; return; }
	# It may ask (its fix list): decline, and it carries on without.
	as_demo "glassctl wait --until needs-you --timeout 180 $id" >/dev/null || fail "round $n: IT agent stuck"
	if ask=$(as_demo 'glassctl --json approvals' | json_field "[a['id'] for a in d if a['task']=='$id'][0]"); then
		as_demo "glassctl deny $ask" >/dev/null
	fi
	as_demo "glassctl wait --timeout 180 $id" >/dev/null || fail "round $n: IT agent didn't end"
	if ((n % 10 == 0)); then
		for t in $(as_demo 'glassctl --json tasks' | json_field '" ".join(t["id"] for t in d if t["state"] in ("done","failed","stopped"))'); do
			as_demo "glassctl dismiss $t" >/dev/null || true
		done
	fi
}

log "soak: $hours h, glassd killed every $crash_every min, power cut every ${cut_every:-0} min; $out"
ready || { fail "glassd isn't answering"; exit 1; }
start=$SECONDS end=$((SECONDS + $(awk "BEGIN { print int($hours * 3600) }")))
next_crash=$((SECONDS + crash_every * 60))
next_cut=$((cut_every > 0 ? SECONDS + cut_every * 60 : 0))
next_verify=$((SECONDS + 1800))
n=0
verify
while ((SECONDS < end)); do
	n=$((n + 1))
	began=$SECONDS
	round "$n"
	sample "$n" $((SECONDS - began))
	if ((crash_every > 0 && SECONDS >= next_crash)); then
		log "killing glassd (round $n)"
		# shellcheck disable=SC2016 # Expanded in the VM.
		as_root 'kill -9 $(cat /var/run/glassd.pid)' || true
		sleep 2
		ready || fail "glassd didn't come back after a crash"
		next_crash=$((SECONDS + crash_every * 60))
	fi
	if ((next_cut > 0 && SECONDS >= next_cut)); then
		log "power cut (round $n)"
		"$vm" power-cut >/dev/null
		"$vm" ssh-close >/dev/null 2>&1 || true
		sleep 20
		"$vm" wait >/dev/null
		ready || fail "glassd didn't come back after a power cut"
		next_cut=$((SECONDS + cut_every * 60))
	fi
	if ((SECONDS >= next_verify)); then
		verify
		next_verify=$((SECONDS + 1800))
	fi
done
verify
sample "$n" 0

# Within each run of glassd (one pid), memory and open files once settled (from its third
# sample on) against where it ended.
python3 - "$out/metrics.csv" "$out" <<'PY' | tee -a "$out/soak.log"
import csv, sys, statistics
rows = list(csv.DictReader(open(sys.argv[1])))
runs = {}
for r in rows:
    runs.setdefault(r["pid"], []).append(r)
bad = []
for pid, rs in runs.items():
    if len(rs) < 6:
        continue
    settled, last = rs[2], rs[-1]
    for key, limit in (("rss_kb", 3), ("fds", 4)):
        a, b = int(settled[key]), int(last[key])
        if a and b > limit * a:
            bad.append(f"glassd {pid}: {key} {a} -> {b}")
took = [int(r["round_s"]) for r in rows if r["round_s"] != "0"]
print(f"rounds: {len(took)}, glassd runs: {len(runs)}, round time median {statistics.median(took) if took else 0} s, max {max(took) if took else 0} s")
print(f"glassd memory: {rows[2]['rss_kb'] if len(rows) > 2 else '?'} KB settled, {rows[-1]['rss_kb']} KB at the end; open files {rows[-1]['fds']}; tasks kept {rows[-1]['tasks']}")
print(f"snapshots: {rows[0]['snapshots']} -> {rows[-1]['snapshots']}; audit log {int(rows[0]['audit_bytes'])//1024} -> {int(rows[-1]['audit_bytes'])//1024} KB; pool free {int(rows[-1]['pool_free'])//2**20} MB")
for b in bad:
    print("FAIL grew:", b)
    open(sys.argv[2] + "/failures", "a").write("grew: " + b + "\n")
PY
if [[ -s $out/failures ]]; then
	log "soak: FAILED ($(wc -l <"$out/failures") failures, see $out/failures)"
	exit 1
fi
log "soak: passed, $n rounds in $(((SECONDS - start) / 60)) min"
