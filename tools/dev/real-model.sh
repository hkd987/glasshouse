#!/usr/bin/env bash
# Usage: tools/dev/real-model.sh [openrouter-model]   (default anthropic/claude-sonnet-4.5)
#
# The IT agent and the desktop agent on a real model, in the dev VM's jails, through OpenRouter
# with the key in OPENROUTER_API_KEY. Set that in this environment's settings, never in chat or the
# repo: it goes to the VM over ssh's stdin into a file only root can read, and is never printed.
# Installs a policy naming the model (glass-e2e real-model-policy), runs one task per agent (the IT
# agent's help desk ticket is approved), prints what each did and said and the record of each
# model call, then puts the VM's policy and model secrets back as they were.
#
# It never changes what the VM trusts. Where outbound HTTPS is re-signed by a proxy the VM doesn't
# trust (a cloud session's, /root/.ccr/README.md), the calls fail with "certificate verify
# failed": run it where the VM reaches OpenRouter directly.
set -euo pipefail

repo=$(cd "$(dirname "$0")/../.." && pwd)
vm=$repo/os/freebsd/scripts/vm.sh
model=${1:-anthropic/claude-sonnet-4.5}
etc=/usr/local/etc/glasshouse
e2e=/usr/local/libexec/glasshouse-tests/glass-e2e

die() {
	echo "real-model: $*" >&2
	exit 1
}
[[ -n ${OPENROUTER_API_KEY:-} ]] ||
	die "OPENROUTER_API_KEY isn't set: add it in this environment's settings"
[[ $model =~ ^[A-Za-z0-9._:/-]+$ ]] || die "'$model' isn't an OpenRouter model id"
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

"$vm" run "set -e
for f in policy.toml model-secrets.toml; do
	if [ -e $etc/\$f ]; then mv $etc/\$f $etc/\$f.real-model-saved; fi
done
$e2e real-model-policy '$model' >$etc/policy.toml"
trap put_back EXIT
printf '[openrouter]\napi_key = "%s"\n' "$OPENROUTER_API_KEY" |
	"$vm" ssh "umask 077; cat >$etc/model-secrets.toml"
"$vm" run "$restart"

# The tasks run as demo; this side only reads what they print.
"$vm" ssh "cat >/tmp/real-model-tasks.sh" <<'EOF'
set -e
short() { sed -n 's/.*(\([0-9a-f]*\))\. Watch.*/\1/p'; }
dir=$HOME/RealModelCheck
mkdir -p "$dir"
printf 'Q4 plan: ship the desktop agent to the pilot org by November 14.\n' >"$dir/plan.txt"
printf 'Call Ana about the launch date. Book the demo room for Thursday.\n' >"$dir/todo.txt"

echo "== desktop agent"
desk=$(glassctl run desktop-agent --read "$dir" -y \
	"What's in my RealModelCheck folder? Read each file and tell me in a sentence what it says." | short)
glassctl wait "$desk" --timeout 180 >/dev/null || true
glassctl show "$desk" | sed -n '/^Answer/,/^Activity/p'
glassctl audit "$desk" | grep -E 'Ask |files\.|Finished|Failed' || true

echo "== IT agent"
it=$(glassctl run it-agent -y "My computer has been slow all morning" | short)
for i in $(jot 360); do
	shown=$(glassctl show "$it")
	ask=$(printf '%s\n' "$shown" | sed -n 's/.*glassctl approve \([0-9a-f]*\).*/\1/p' | head -1)
	[ -n "$ask" ] && glassctl approve "$ask" >/dev/null
	printf '%s\n' "$shown" | head -1 | grep -qE ' (Done|Failed|Stopped) ' && break
	sleep 0.5
done
glassctl show "$it" | head -3
glassctl audit "$it" | grep -E 'Ask |system\.|helpdesk|Finished|Failed' || true
EOF
"$vm" run "su demo -c 'sh /tmp/real-model-tasks.sh'"
