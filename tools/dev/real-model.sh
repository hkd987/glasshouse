#!/usr/bin/env bash
# Usage: tools/dev/real-model.sh [--host] [--provider openrouter|anthropic] [model]
#
# The IT agent and the desktop agent on a real model:
#   openrouter (the default): model anthropic/claude-sonnet-4.5 unless named, key in OPENROUTER_API_KEY
#   anthropic (ADR 0043): model claude-opus-5-5 unless named, key in GLASS_ANTHROPIC_API_KEY (not
#     ANTHROPIC_API_KEY, which a Claude Code session in the same environment would use itself)
# Set the key in this environment's settings, never in chat or the repo: it goes
# into a model-secrets file only its owner can read (over ssh's stdin, for the VM) and is never
# printed. Writes a policy naming the model (glass-e2e real-model-policy), runs one task per agent
# (the IT agent's help desk ticket is approved), prints what each did and said and the record of
# each model call, then puts everything back.
#
# By default it runs in the dev VM's jails and restores the VM's policy and model secrets after.
# It never changes what the VM trusts: where outbound HTTPS is re-signed by a proxy the VM doesn't
# trust (a cloud session's, /root/.ccr/README.md), the calls fail with "certificate verify
# failed". --host runs on this machine instead (tools/dev/host-run.sh, NOTHING CONTAINED), which
# trusts what this machine trusts: the way to try a real model from a cloud session.
set -euo pipefail

repo=$(cd "$(dirname "$0")/../.." && pwd)
host=
provider=openrouter
die() {
	echo "real-model: $*" >&2
	exit 1
}
while [[ ${1:-} == --* ]]; do
	case $1 in
	--host) host=1 ;;
	--provider)
		provider=${2:-}
		shift
		;;
	*) die "unknown option $1" ;;
	esac
	shift
done
case $provider in
openrouter)
	model=${1:-anthropic/claude-sonnet-4.5}
	keyvar=OPENROUTER_API_KEY
	;;
anthropic)
	model=${1:-claude-opus-5-5}
	keyvar=GLASS_ANTHROPIC_API_KEY
	;;
*) die "--provider is openrouter or anthropic" ;;
esac
key=${!keyvar:-}
[[ -n $key ]] || die "$keyvar isn't set: add it in this environment's settings"
[[ $model =~ ^[A-Za-z0-9._:/-]+$ ]] || die "'$model' isn't a model id"

# One task per agent, as whoever runs it; DIR is a folder the desktop agent may read.
# shellcheck disable=SC2016
tasks='set -e
short() { sed -n "s/.*(\([0-9a-f]*\))\. Watch.*/\1/p"; }
mkdir -p "$DIR"
printf "Q4 plan: ship the desktop agent to the pilot org by November 14.\n" >"$DIR/plan.txt"
printf "Call Ana about the launch date. Book the demo room for Thursday.\n" >"$DIR/todo.txt"

echo "== desktop agent"
desk=$(glassctl run desktop-agent --read "$DIR" -y \
	"What is in my RealModelCheck folder? Read each file and tell me in a sentence what it says." | short)
glassctl wait "$desk" --timeout 180 >/dev/null || true
glassctl show "$desk" | sed -n "/^Answer/,/^Activity/p"
glassctl audit "$desk" | grep -E "Ask |files\.|Finished|Failed" || true

echo "== IT agent"
it=$(glassctl run it-agent -y "My computer has been slow all morning" | short)
i=0
while [ $i -lt 360 ]; do
	shown=$(glassctl show "$it")
	ask=$(printf "%s\n" "$shown" | sed -n "s/.*glassctl approve \([0-9a-f]*\).*/\1/p" | head -1)
	[ -n "$ask" ] && glassctl approve "$ask" >/dev/null
	printf "%s\n" "$shown" | head -1 | grep -qE " (Done|Failed|Stopped) " && break
	sleep 0.5
	i=$((i + 1))
done
glassctl show "$it" | head -3
glassctl audit "$it" | grep -E "Ask |system\.|helpdesk|Finished|Failed" || true'

if [[ -n $host ]]; then
	dev=$repo/build/dev-real-model
	run=$(mktemp -d /tmp/glass-real-model.XXXXXX)
	bin=${CARGO_TARGET_DIR:-$repo/target}/debug
	mkdir -p "$dev"
	cargo build -q -p glass-e2e
	"$bin/glass-e2e" real-model-policy "$provider" "$model" >"$dev/real-policy.toml"
	(umask 077 && printf '[%s]\napi_key = "%s"\n' "$provider" "$key" >"$dev/model-secrets.toml")
	GLASS_DEV_DIR=$dev GLASS_DEV_RUN=$run GLASS_DEV_POLICY=$dev/real-policy.toml \
		GLASS_DEV_MODEL_SECRETS=$dev/model-secrets.toml "$repo/tools/dev/host-run.sh" >"$dev/glassd.log" 2>&1 &
	glassd=$!
	# shellcheck disable=SC2064
	trap "kill $glassd 2>/dev/null; rm -rf '$dev/model-secrets.toml' '$dev/home/RealModelCheck' '$run'" EXIT
	export GLASS_CONTROL_SOCKET=$run/control.sock PATH=$bin:$PATH
	for _ in $(seq 600); do
		glassctl status >/dev/null 2>&1 && break
		kill -0 "$glassd" 2>/dev/null || die "glassd stopped; see $dev/glassd.log"
		sleep 0.5
	done
	DIR=$dev/home/RealModelCheck sh -c "$tasks"
	exit
fi

vm=$repo/os/freebsd/scripts/vm.sh
etc=/usr/local/etc/glasshouse
e2e=/usr/local/libexec/glasshouse-tests/glass-e2e
"$vm" run true >/dev/null 2>&1 || die "the VM isn't answering: make vm-up vm-wait vm-deploy"

# glass-e2e writes the policy; this checkout's, so it knows real-model-policy.
cargo build -q --profile vm --target x86_64-unknown-freebsd -p glass-e2e
"$vm" run "mkdir -p ${e2e%/*}"
"$vm" scp-in "$repo/target/x86_64-unknown-freebsd/vm/glass-e2e" "$e2e"

# Expanded in the VM, not here.
# shellcheck disable=SC2016
restart='service glassd restart </dev/null >/dev/null 2>&1
for i in $(jot 120); do glassctl status >/dev/null 2>&1 && break; sleep 0.5; done'

put_back() {
	"$vm" run "rm -f $etc/policy.toml $etc/model-secrets.toml /tmp/real-model-tasks.sh
rm -rf /home/demo/RealModelCheck
for f in policy.toml model-secrets.toml; do
	if [ -e $etc/\$f.real-model-saved ]; then mv $etc/\$f.real-model-saved $etc/\$f; fi
done
$restart" >/dev/null || echo "real-model: couldn't put the VM back; look in $etc" >&2
}

# Set first: anything after this point leaves the VM as it found it.
trap put_back EXIT
"$vm" run "set -e
for f in policy.toml model-secrets.toml; do
	if [ -e $etc/\$f ]; then mv $etc/\$f $etc/\$f.real-model-saved; fi
done
$e2e real-model-policy '$provider' '$model' >$etc/policy.toml"
printf '[%s]\napi_key = "%s"\n' "$provider" "$key" |
	"$vm" ssh "umask 077; cat >$etc/model-secrets.toml"
"$vm" run "$restart"

# The tasks run as demo; this side only reads what they print.
printf '%s\n' "$tasks" | "$vm" ssh "cat >/tmp/real-model-tasks.sh"
"$vm" run "su demo -c 'DIR=/home/demo/RealModelCheck sh /tmp/real-model-tasks.sh'"
