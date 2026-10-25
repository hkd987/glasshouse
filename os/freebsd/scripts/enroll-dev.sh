#!/usr/bin/env bash
# Usage: enroll-dev.sh [device]    enroll the dev VM with `make console-dev` (default device glasshouse-vm)
#        enroll-dev.sh --undo      take it out again
#
# Enrolls the dev VM's glassd with the development admin console running on
# this host (ADR 0023), so policy, the catalog and what IT changes reach the
# VM as they would a managed computer. The VM talks to a console over HTTPS
# only, so this puts tools/dev/console-tls.py in front of the console (the
# part our web servers play in production), with a development certificate
# for 10.0.2.2 (the host, as the VM sees it) that the VM is told to trust.
# Needs `make console-dev` running (its state in build/console).
#
# --undo takes out the settings, the token, the development CA it told the
# VM to trust, and what the console left on the VM (its kept releases and
# the policy file it wrote), so the VM is back to the shipped default
# policy and trusts what it did before. `make e2e-vm` refuses to run while
# it's enrolled.
set -euo pipefail
# shellcheck source=lib.sh
. "$(dirname "$0")/lib.sh"

state=$REPO_ROOT/build/console
tls=$state/tls
port=7443
conf=/usr/local/etc/glasshouse/glassd.toml
token=/usr/local/etc/glasshouse/enrollment.token
ca=/usr/local/share/certs/glasshouse-dev-console.pem
begin='# --- enroll-dev.sh: begin'
end='# --- enroll-dev.sh: end'

vm_running || die "VM is not running (vm.sh up && vm.sh wait)"

if [[ ${1:-} == --undo ]]; then
	vm_ssh "sed -i '' '/^$begin/,/^$end/d' $conf; rm -f $token; rm -rf /var/db/glasshouse/managed
	rm -f /usr/local/etc/glasshouse/policy.toml $ca; certctl rehash; service glassd restart >/dev/null"
	log "the VM is no longer enrolled"
	exit 0
fi
device=${1:-glasshouse-vm}

[[ -f $state/enrollment.json ]] || die "no development console in ${state#"$REPO_ROOT"/} (make console-dev)"
field() { python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))[sys.argv[2]])' "$state/enrollment.json" "$1"; }
curl -fsS -o /dev/null "$(field url)/" || die "the console at $(field url) isn't answering (make console-dev)"

# A development CA and the console's certificate, made once.
dev_tls "$tls"

# HTTPS in front of the console, unless it's there already.
if ! curl -fsS --cacert "$tls/ca.pem" -o /dev/null "https://127.0.0.1:$port/" 2>/dev/null; then
	nohup python3 "$REPO_ROOT/tools/dev/console-tls.py" "$tls/console.pem" "$tls/console.key" \
		"$port" "$(field url | sed 's|.*:||; s|/.*||')" >"$tls/console-tls.log" 2>&1 &
	for _ in $(seq 50); do
		curl -fsS --cacert "$tls/ca.pem" -o /dev/null "https://127.0.0.1:$port/" 2>/dev/null && break
		sleep 0.1
	done
fi

vm_ssh "mkdir -p ${ca%/*}"
vm_scp "$tls/ca.pem" "$ca"
field token | vm_ssh "umask 077; cat >$token"
vm_ssh "certctl rehash; sed -i '' '/^$begin/,/^$end/d' $conf; cat >>$conf" <<TOML
$begin
[managed]
url = "https://10.0.2.2:$port"
org = "$(field org)"
org_keys = ["$(field org_key)"]
token_file = "$token"
device = "$device"
$end
TOML
vm_ssh "service glassd restart >/dev/null"
log "the VM is enrolled as $device with the console at https://10.0.2.2:$port"
