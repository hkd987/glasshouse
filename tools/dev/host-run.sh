#!/usr/bin/env bash
# Run glassd on a development host (Linux or macOS) with the insecure host
# sandbox and copy snapshots. NOTHING IS CONTAINED: agents run as ordinary
# processes. Use it to iterate on the runtime, CLI and agents; test
# containment in the FreeBSD VM (`make vm-deploy e2e-vm`).
#
# usage: tools/dev/host-run.sh            # build, seed, run in the foreground
#        tools/dev/host-run.sh --build    # build what it runs, and stop there
#        GLASS_CONTROL_SOCKET=build/dev/run/control.sock glassctl ...
#        make apps-dev                     # the apps, on http://<app>.localhost:7300
#
# GLASS_DEV_DIR puts the state elsewhere (the apps' e2e suite uses its own),
# GLASS_DEV_RUN the sockets (a Unix socket's path must stay under 104 bytes);
# GLASS_DEV_SKIP_BUILD=1 runs what is already built: what `--build` built.
# The test suites that start it that way (make apps-test, console-test)
# build with `--build` first, so this file's list is the only one.
# GLASS_DEV_ENROLL=build/console/enrollment.json enrolls this glassd with a
# development admin console (make console-dev, ADR 0023) as device
# GLASS_DEV_DEVICE (default dev-host).
# GLASS_DEV_SCRIPTED_MODEL=1 gives the default policy the scripted model
# (no network), which calls the tool a prompt names: `Use mail_draft {...}`.
# GLASS_DEV_POLICY uses that policy file instead of the default policy, and
# GLASS_DEV_MODEL_SECRETS that model-secrets file (tools/dev/real-model.sh).
# GLASS_DEV_COMPUTERS=1 gives each agent a computer of its own on Linux
# namespaces (ADR 0066) instead of the insecure host backend: what
# tools/dev/agents-run.sh (make agents-dev) runs.
# GLASS_DEV_OS_COMPUTERS=1 (with GLASS_DEV_COMPUTERS=1) makes each person's
# computer a Glasshouse OS VM (ADR 0072): a layer over the dev VM's disk,
# its desktop shown and driven from the Agents app. The dev VM must be down
# while they run (make vm-down): its disk is their image. Emulated without
# KVM, so a computer takes minutes to boot.
# GLASS_DEV_STORE=s3 puts sleeping computers in S3 on this machine instead
# of a folder (make aws-local, deploy/local): moto's bucket under the
# stack's KMS key, signed with the stand-in instance role's keys, and
# reports agent computer hours to the stand-in Metering Service. Only the
# stand-ins: any AWS keys in the environment are kept from glassd.
# GLASS_DEV_HOST names this host to the stand-in metadata service (default
# dev), for tests of a host being replaced.
# GLASS_DEV_APPS_OFF=mail,calendar turns those apps off for the org, and
# GLASS_DEV_POLICY_EXTRA names a file of policy to add, such as a group
# that turns one back on (ADR 0070).
# GLASS_DEV_PEOPLE=1 makes it an agent host for many people (ADR 0067): the
# front, as the account glass-agents, acts for whoever the stand-in load
# balancer signed in (make aws-local), and glassd checks its signed claims
# with the stand-in's public key.
set -euo pipefail

repo=$(cd "$(dirname "$0")/../.." && pwd)
dev="${GLASS_DEV_DIR:-$repo/build/dev}"
run="${GLASS_DEV_RUN:-$dev/run}"
bin="${CARGO_TARGET_DIR:-$repo/target}/debug"

# Everything this script starts or names in the configuration it writes.
build() {
	cargo build --manifest-path "$repo/Cargo.toml" -p glassd -p glassctl -p glass-demo-agent -p glass-probe \
		-p glass-mcp -p glass-app-bridge -p glass-skill-evals -p glass-it-agent -p glass-hardware-agent -p glass-desktop-agent \
		-p glass-claude-runner -p glass-app-try
	cargo build --manifest-path "$repo/Cargo.toml" -p glass-e2e --bin glass-fake-wpa
}
if [ "${1:-}" = "--build" ]; then
	build
	exit 0
fi
if [ -z "${GLASS_DEV_SKIP_BUILD:-}" ]; then
	build
fi

# A Unix socket's path stops at about 104 bytes. The longest here is an
# agent's, in its task's sandbox under the state folder: past the limit,
# every task fails before its agent starts, with nothing that says a folder
# is too deep. Said here instead, before anything runs.
agent_socket="$dev/state/sandboxes/glass-00000000/run/glasshouse/agent.sock"
if [ "${#agent_socket}" -gt 103 ] || [ "$(printf %s "$run/control.sock" | wc -c)" -gt 103 ]; then
	echo "host-run: $dev (or $run) is too deep: an agent's socket under it would have a path of ${#agent_socket} bytes, and a Unix socket's stops at 103." >&2
	echo "host-run: set GLASS_DEV_DIR (and GLASS_DEV_RUN) to a shorter folder, such as /tmp/glass-dev." >&2
	exit 1
fi

mkdir -p "$dev/home" "$dev/state" "$run"
# Our apps' source as images lay it out (ADR 0032), linked to the workspace's;
# remixing off removes the links, never what they point at.
mkdir -p "$dev/source"
ln -sfn "$repo/apps" "$dev/source/apps"
ln -sfn "$repo/apps/sdk" "$dev/source/sdk"
if [ ! -d "$dev/home/Invoices" ]; then
	mkdir -p "$dev/home/Invoices" "$dev/home/Reports" "$dev/home/Finance"
	cp "$repo"/os/freebsd/overlay/usr/local/share/glasshouse/demo/Invoices/*.txt "$dev/home/Invoices/"
	printf 'quarter,revenue\nQ3,1250000\n' >"$dev/home/Finance/q3-board.csv"
fi

backend=host-insecure
if [ -n "${GLASS_DEV_COMPUTERS:-}" ]; then
	backend=computers
fi

people=""
if [ -n "${GLASS_DEV_PEOPLE:-}" ]; then
	[ -s "$repo/build/local-aws/env" ] || {
		echo "host-run: GLASS_DEV_PEOPLE=1 needs the stand-in load balancer: make aws-local" >&2
		exit 1
	}
	# shellcheck source=/dev/null
	. "$repo/build/local-aws/env"
	people="
# The front acts for people the load balancer signed in (ADR 0067).
[people]
front_user = \"glass-agents\"
load_balancer = \"$GLASS_LOCAL_SIGNER\"
region = \"$GLASS_LOCAL_REGION\"
public_keys_url = \"$GLASS_LOCAL_PUBLIC_KEYS\""
fi

store="store = \"folder\"
folder = \"$dev/computer-store\""
metering=""
schedules=""
case "${GLASS_DEV_STORE:-folder}" in
folder) ;;
s3)
	[ -s "$repo/build/local-aws/env" ] || {
		echo "host-run: GLASS_DEV_STORE=s3 needs AWS on this machine: make aws-local" >&2
		exit 1
	}
	# shellcheck source=/dev/null
	. "$repo/build/local-aws/env"
	store="store = \"s3\"
bucket = \"$GLASS_LOCAL_BUCKET\"
region = \"$GLASS_LOCAL_REGION\"
kms_key_arn = \"$GLASS_LOCAL_KMS_KEY\"
endpoint = \"$GLASS_LOCAL_S3\""
	metering="
# Agent computer hours, to the stand-in Metering Service (deploy/local).
[metering]
enabled = true
product_code = \"$GLASS_LOCAL_PRODUCT_CODE\"
region = \"$GLASS_LOCAL_REGION\"
endpoint = \"$GLASS_LOCAL_METERING\""
	schedules="
# People's schedules, in DynamoDB on this machine (ADR 0069).
[schedules]
store = \"dynamodb\"
table = \"$GLASS_LOCAL_SCHEDULES_TABLE\"
region = \"$GLASS_LOCAL_REGION\"
endpoint = \"$GLASS_LOCAL_DYNAMODB\""
	unset AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY AWS_SESSION_TOKEN AWS_PROFILE
	# This host's own metadata (its lifecycle state), by its name.
	export AWS_EC2_METADATA_SERVICE_ENDPOINT=$GLASS_LOCAL_IMDS/${GLASS_DEV_HOST:-dev}
	;;
*)
	echo "host-run: GLASS_DEV_STORE is folder or s3" >&2
	exit 1
	;;
esac

hypervisor=""
qemu=""
if [ -n "${GLASS_DEV_OS_COMPUTERS:-}" ]; then
	image=$repo/build/vm/disk.qcow2
	[ -f "$image" ] || {
		echo "host-run: GLASS_DEV_OS_COMPUTERS needs the dev VM's disk ($image): make test-env" >&2
		exit 1
	}
	if [ -s "$repo/build/vm/qemu.pid" ] && kill -0 "$(cat "$repo/build/vm/qemu.pid")" 2>/dev/null; then
		echo "host-run: the dev VM is running, and its disk is these computers' image: make vm-down" >&2
		exit 1
	fi
	# The firmware the dev VM boots (os/freebsd/scripts/lib.sh).
	code=/usr/share/OVMF/OVMF_CODE_4M.fd
	[ -f /usr/share/OVMF/OVMF_CODE_4M.secboot.fd ] && code=/usr/share/OVMF/OVMF_CODE_4M.secboot.fd
	hypervisor="hypervisor = \"qemu\"
memory_mib = ${GLASS_DEV_OS_MEMORY:-3072}
vcpus = 2"
	qemu="
# Each person's computer is a Glasshouse OS VM (ADR 0072).
[sandbox.computers.qemu]
image = \"$image\"
firmware_code = \"$code\"
firmware_vars = \"/usr/share/OVMF/OVMF_VARS_4M.fd\"
ssh_key = \"$repo/build/vm/ssh/id_ed25519\""
fi

cat >"$dev/glassd.toml" <<TOML
# Generated by tools/dev/host-run.sh. Development only.
control_socket = "$run/control.sock"
state_dir = "$dev/state"
policy = "${GLASS_DEV_POLICY:-$dev/policy.toml}"
model_secrets = "${GLASS_DEV_MODEL_SECRETS:-$dev/model-secrets.toml}"
# The apps (ADR 0024): their socket, their manifests, and everyone's app
# data (~/Mail, ~/Calendar) in the dev home instead of their real one.
app_socket = "$run/apps.sock"
apps_dir = "$repo/apps"
dev_home = "$dev/home"
# Take over continues in the user's own Claude Code (glass-mcp).
harness = "claude-code"

[sandbox]
backend = "$backend"
allow_insecure = true
agent_dirs = ["$bin"]

# Agent computers (GLASS_DEV_COMPUTERS=1, ADR 0066) sleep in a folder
# standing in for S3, or S3 on this machine (GLASS_DEV_STORE=s3), after
# GLASS_DEV_IDLE_MINUTES unused.
[sandbox.computers]
$hypervisor
$store
sleep_after_idle_minutes = ${GLASS_DEV_IDLE_MINUTES:-30}
$qemu
$metering
$schedules
$people

[snapshots]
backend = "copy"

# Wi-Fi (CF-12): a stand-in wpa_supplicant with networks in the air
# (glass-fake-wpa), and the organization's passwords, root's file on a
# computer, the developer's here.
[wifi]
control_dir = "$run/wpa"
secrets = "$dev/wifi-secrets.toml"

# The screen's brightness (ADR 0060): a file stands in for a backlight.
[display]
backlight = "$dev/backlight"

# Bluetooth (CF-13, ADR 0062): a file stands in for a controller, with a
# keyboard, a mouse, a phone and headphones nearby.
[bluetooth]
stand_in = "$dev/bluetooth.json"

# Remixing and making apps (ADR 0032): the apps workspace's esbuild, its
# source in the layout images have, and what tries each version (ADR 0049).
[studio]
bundler = "$repo/apps/node_modules/@esbuild/linux-x64/bin/esbuild"
tryout = "$bin/glass-app-try"
source = "$dev/source"

[[agent]]
id = "invoice-demo"
name = "Invoice demo"
description = "Scripted demo agent."
program = "$bin/glass-demo-agent"
plan = ["Read invoices", "Write the summary", "Draft a note to AP", "Ask before reminding vendors"]

[[agent]]
id = "desktop-agent"
name = "Desktop agent"
description = "Does what you ask with the tools policy gives it."
program = "$bin/glass-desktop-agent"
plan = ["Work out what you asked with an AI model IT approved", "Use the tools policy gives it", "Tell you what it did and found"]

[[agent]]
id = "desktop-agent-plan"
name = "Desktop agent (your plan)"
description = "The desktop agent, run by your own Claude Code on your Claude plan, with this computer's tools only."
attended = true
driver = "$bin/glass-claude-runner"
model = "Your Claude plan, through your own Claude Code"
plan = ["Start your own Claude Code with only this computer's tools", "Use the tools the rules give it", "Tell you what it did and found"]

[[agent]]
id = "it-agent"
name = "IT agent"
description = "Fixes this computer: network and VPN, storage and slowdowns, from IT's approved list. Opens a help desk ticket with diagnostics when it can't."
program = "$bin/glass-it-agent"
plan = ["Check the parts of this computer your problem points to", "Apply a fix from IT's approved list", "Ask you before restarting anything or opening a help desk ticket"]

[[agent]]
id = "hardware-agent"
name = "Hardware agent"
description = "Writes a fix for a part of this computer that doesn't work as a kernel patch, has it built, and asks you before the next restart tries it."
program = "$bin/glass-hardware-agent"
plan = ["Read this computer's hardware report", "Write the smallest change and have it built", "Ask you before the next restart tries it"]

[[agent]]
id = "skill-evals"
name = "Skill evals"
description = "Runs a skill's evals and keeps the scores on this computer."
program = "$bin/glass-skill-evals"
plan = ["Read the skill", "Ask the model each case", "Keep the scores"]

[[agent]]
id = "claude-code"
name = "Claude Code"
description = "Your own Claude Code, connected through glass-mcp."
attended = true

[[agent]]
id = "containment-probe"
name = "Containment probe"
description = "Tries to escape its sandbox. Expected to FAIL on the host backend."
program = "$bin/glass-probe"
TOML

# Managed by a development admin console: its address, org, key and token.
if [ -n "${GLASS_DEV_ENROLL:-}" ]; then
	enroll() { python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))[sys.argv[2]])' "$GLASS_DEV_ENROLL" "$1"; }
	(umask 077 && enroll token >"$dev/enrollment.token")
	cat >>"$dev/glassd.toml" <<TOML

[managed]
url = "$(enroll url)"
org = "$(enroll org)"
org_keys = ["$(enroll org_key)"]
token_file = "$dev/enrollment.token"
device = "${GLASS_DEV_DEVICE:-dev-host}"
allow_insecure_http = true
TOML
fi

# The default policy labels /home/*/Finance; label the dev home's too.
apps_off=""
if [ -n "${GLASS_DEV_APPS_OFF:-}" ]; then
	apps_off="apps_off = [\"${GLASS_DEV_APPS_OFF//,/\", \"}\"]"
fi
{
	"$bin/glassctl" policy default | awk -v off="$apps_off" '{ print } /^\[org\]$/ && off != "" { print off }'
	printf '\n[[label]]\npath = "%s/Finance"\nlabel = "confidential"\n' "$dev/home"
	if [ -n "${GLASS_DEV_POLICY_EXTRA:-}" ]; then
		echo
		cat "$GLASS_DEV_POLICY_EXTRA"
	fi
	if [ -n "${GLASS_DEV_SCRIPTED_MODEL:-}" ]; then
		cat <<'MODELS'

[models]
default = "echo"

[[models.provider]]
id = "scripted"
kind = "scripted"
name = "Scripted (no network)"

[[models.model]]
id = "echo"
provider = "scripted"
model = "echo-1"
name = "Echo"
usd_per_mtok_in = 3.0
usd_per_mtok_out = 15.0
MODELS
	fi
} >"$dev/policy.toml"

# The stand-in wpa_supplicant, gone when glassd (this process, after exec) is.
"$bin/glass-fake-wpa" "$run/wpa" &
(umask 077 && printf '[networks."Northwind Staff"]\npassword = "staff-password-not-real"\n' >"$dev/wifi-secrets.toml")
cat >"$dev/bluetooth.json" <<'BT'
{"on": true, "nearby": [
  {"address": "00:1f:20:3a:4b:5c", "name": "Keyboard K380", "class": 9536},
  {"address": "00:1f:20:3a:4b:5d", "name": "MX Anywhere 3", "class": 9600},
  {"address": "00:80:37:29:19:a4", "name": "Dana's phone", "class": 5374468},
  {"address": "00:1b:66:01:02:03", "name": "WH-1000XM4", "class": 2360324}
]}
BT

echo "glassd ($backend) — in another shell:"
echo "  export GLASS_CONTROL_SOCKET=$run/control.sock"
echo "  $bin/glassctl run invoice-demo 'Reconcile invoices' --read $dev/home/Invoices --write $dev/home/Reports"
echo "  make apps-dev    # Mail and Calendar at http://mail.localhost:7300 and http://calendar.localhost:7300"
exec "$bin/glassd" --config "$dev/glassd.toml"
