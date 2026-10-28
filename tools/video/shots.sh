#!/usr/bin/env bash
# The walkthrough video's pictures, into build/video/shots (tools/video/make.py
# builds the video from them):
#   desktop      the desktop shell's own screens with sample data (make
#                shell-preview);
#   console      the admin console, live on its 500-device fixture: IT's
#                tiers, Calendar turned off, the preview of who it reaches;
#   agents       Glasshouse Agents, live on this machine (scripted model, no
#                network): three agents, work done, an approval waiting, a
#                schedule; the web app and glass;
#   vm           the computer itself, live in the dev VM: undo, the audit
#                record and its check (the VM must be up and deployed);
#   tests        what the negative suites printed, from their logs.
# usage: tools/video/shots.sh [desktop] [console] [agents] [vm] [tests]
set -euo pipefail

repo=$(cd "$(dirname "$0")/../.." && pwd)
shots=$repo/build/video/shots
bin="${CARGO_TARGET_DIR:-$repo/target}/debug"
shot() { "$repo/tools/screenshot/termshot.py" --width 1180 "$@"; }
mkdir -p "$shots"
parts=("$@")
[ ${#parts[@]} -gt 0 ] || parts=(desktop console agents vm tests)

for part in "${parts[@]}"; do
	case "$part" in
	desktop)
		make -C "$repo" shell-preview >/dev/null
		for n in desktop command-bar expanded-email expanded-rollback; do
			cp "$repo/build/shell-preview/$n.png" "$shots/$n.png"
		done
		;;
	console)
		cargo build --manifest-path "$repo/Cargo.toml" -p glass-console
		make -C "$repo" apps-build >/dev/null
		state=$repo/build/video/console
		rm -rf "$state"
		"$bin/glass-console" --dev --state "$state" --web "$repo/apps/console/dist" --fixture 500,50 \
			>"$repo/build/video/console.log" 2>&1 &
		console=$!
		for _ in $(seq 1 100); do curl -s -o /dev/null http://127.0.0.1:7400/ && break; sleep 0.2; done
		# Pat is one of the fixture's IT admins.
		(cd "$repo" && node tools/video/capture.mjs console http://127.0.0.1:7400/ pat "$shots")
		kill "$console"
		;;
	agents)
		dev=$repo/build/video/agents
		rm -rf "$dev" "$dev-computers"
		mkdir -p "$dev"
		# Claude Haiku does the work (GLASS_ANTHROPIC_API_KEY; about a tenth
		# of a cent a task), asked in plain words as a person would.
		GLASS_DEV_DIR=$dev GLASS_DEV_RUN=$dev/run GLASS_DEV_SKIP_BUILD=1 GLASS_AGENTS_TOKEN_FILE=$dev/agents.json \
			GLASS_AGENTS_MODEL=anthropic GLASS_AGENTS_MODEL_ID=claude-haiku-5-5 \
			GLASS_AGENTS_NAME="Dana Alvarez" setsid "$repo/tools/dev/agents-run.sh" >"$dev/stack.log" 2>&1 &
		stack=$!
		trap 'kill -TERM -- "-$stack" 2>/dev/null || true' EXIT
		for _ in $(seq 1 300); do [ -s "$dev/agents.json" ] && break; sleep 0.1; done
		field() { python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))[sys.argv[2]])' "$dev/agents.json" "$1"; }
		GLASS_AGENTS_URL=$(field url)
		GLASS_AGENTS_TOKEN=$(field token)
		export GLASS_AGENTS_URL GLASS_AGENTS_TOKEN PATH="$bin:$PATH"
		glass new desktop-agent --name Reconciler >/dev/null
		glass new desktop-agent --name Researcher >/dev/null
		glass new desktop-agent --name "Month-end closer" >/dev/null
		glass ask reconciler "Make ~/ledger/march.csv with these unmatched March invoices: Northwind 18,420.00, Contoso 2,310.50, Fabrikam 912.00. Then tell me the total." >/dev/null
		glass ask researcher "Write ~/research/northwind.md: three questions to ask Northwind before their renewal, a line each." >/dev/null
		glass ask --no-wait month-end-closer "Email ap@northwind.example: seven of their March invoices didn't match a payment; ask them to send remittance details by Friday." >/dev/null
		for _ in $(seq 1 120); do glass approvals | grep -q "glass approve" && break; sleep 1; done
		glass schedule reconciler "weekdays 09:00" "Match yesterday's invoices and tell me what's left" --zone Europe/London >/dev/null
		(cd "$repo" && node tools/video/capture.mjs agents "$GLASS_AGENTS_URL/?token=$GLASS_AGENTS_TOKEN" "$shots")
		shot --out "$shots/schedules.png" --title "glass: your agents, their questions, and work on a schedule" -- \
			"glass agents" "glass approvals" \
			"glass schedule researcher 'mon,thu 08:30' 'Brief me on what changed with Northwind' --zone America/New_York" \
			"glass schedules"
		kill -TERM -- "-$stack" 2>/dev/null || true
		trap - EXIT
		;;
	vm)
		# The invoice demo writes its summary to ~/Reports, asks before
		# reminding vendors, is told no, and its changes are undone.
		vm() { "$repo/os/freebsd/scripts/vm.sh" run "su demo -c '$1'"; }
		id() { python3 -c 'import json,sys; d=json.load(sys.stdin); d=d[0] if isinstance(d,list) else d; print(d["id"])'; }
		# Its report is new, so undo has something to take back.
		vm "rm -f ~/Reports/reconciliation.csv"
		task=$(vm 'glassctl --json run invoice-demo "Reconcile the October invoices and remind vendors" --read ~/Invoices --write ~/Reports --yes' | id)
		vm "glassctl wait $task --until needs-you" >/dev/null
		ask=$(vm "glassctl --json approvals" | id)
		vm "glassctl deny $ask" >/dev/null
		vm "glassctl wait $task" >/dev/null || true
		shot --out "$shots/undo.png" --title "Undo: everything the task changed, put back" --vm --user demo -- \
			"ls ~/Reports" "glassctl undo $task --dry-run" "glassctl undo $task" "ls ~/Reports"
		shot --out "$shots/audit.png" --title "The record: each step, the rule that decided it, and a check" --vm --user root -- \
			"glassctl audit $task | head -16" "glassctl verify"
		;;
	tests)
		logs=${GLASS_VIDEO_TEST_LOGS:?the folder with negative.log and schedules.log}
		mkdir -p "$repo/build/video/tests"
		cp "$logs/negative.log" "$logs/schedules.log" "$repo/build/video/tests/"
		(cd "$repo/build/video/tests" && shot --out "$shots/negative.png" --title "What must fail, does: make aws-local-test" -- \
			"grep -E '^  (ok|FAIL)' negative.log | head -22" "grep -cE '^  (ok|FAIL)' negative.log schedules.log" "grep -c FAIL negative.log schedules.log")
		;;
	*)
		echo "shots: desktop, console, agents, vm or tests" >&2
		exit 2
		;;
	esac
done
ls "$shots"
