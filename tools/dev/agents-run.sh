#!/usr/bin/env bash
# Glasshouse Agents on this machine (ADR 0066): glassd with each agent on a
# computer of its own (Linux namespaces), and the front, glass-agents, with
# the web app. Development only: the front acts for whoever runs it.
#
# usage: tools/dev/agents-run.sh           # or: make agents-dev
#        then open the link it prints, or use glass (it reads
#        ~/.config/glasshouse/agents.json)
#
# GLASS_AGENTS_MODEL=anthropic runs the agents on Claude (claude-opus-5-5,
# or GLASS_AGENTS_MODEL_ID), its key from GLASS_ANTHROPIC_API_KEY, kept in
# a file only its owner reads and removed on exit; otherwise the scripted
# model (no network). GLASS_DEV_DIR, GLASS_DEV_RUN, GLASS_AGENTS_LISTEN
# (default 127.0.0.1:7500) and GLASS_AGENTS_TOKEN_FILE (default
# ~/.config/glasshouse/agents.json) put things elsewhere;
# GLASS_DEV_SKIP_BUILD=1 runs what is built. GLASS_DEV_STORE=s3 keeps
# computers in S3 on this machine (make aws-local, tools/dev/host-run.sh).
# GLASS_AGENTS_IDENTITY=load-balancer runs it as an agent host for many
# people (ADR 0067): the front as the account glass-agents (made if
# missing; root only), behind the stand-in load balancer at
# $GLASS_LOCAL_LOAD_BALANCER, which sends people to GLASS_AGENTS_LISTEN.
# GLASS_DEV_OS_COMPUTERS=1 makes each person's computer a Glasshouse OS VM
# whose desktop the Agents app shows and takes over (ADR 0072;
# tools/dev/host-run.sh says what it needs).
set -euo pipefail

repo=$(cd "$(dirname "$0")/../.." && pwd)
dev="${GLASS_DEV_DIR:-$repo/build/agents-dev}"
run="${GLASS_DEV_RUN:-$dev/run}"
bin="${CARGO_TARGET_DIR:-$repo/target}/debug"
listen="${GLASS_AGENTS_LISTEN:-127.0.0.1:7500}"

if [ -z "${GLASS_DEV_SKIP_BUILD:-}" ]; then
	cargo build --manifest-path "$repo/Cargo.toml" -p glass-agents -p glass -p glass-e2e
	make -C "$repo" apps-build >/dev/null
fi

mkdir -p "$dev" "$run"
export GLASS_DEV_DIR=$dev GLASS_DEV_RUN=$run GLASS_DEV_COMPUTERS=1
identity=${GLASS_AGENTS_IDENTITY:-local}
as_front=()
if [ "$identity" = load-balancer ]; then
	# The front's own account, as the stack makes it: the one that may say
	# who a request is for.
	if ! id glass-agents >/dev/null 2>&1; then
		useradd --system --no-create-home --shell /usr/sbin/nologin glass-agents
	fi
	export GLASS_DEV_PEOPLE=1
	as_front=(setpriv --reuid=glass-agents --regid=glass-agents --init-groups)
fi
case "${GLASS_AGENTS_MODEL:-scripted}" in
anthropic)
	key=${GLASS_ANTHROPIC_API_KEY:-}
	[ -n "$key" ] || {
		echo "agents-run: GLASS_ANTHROPIC_API_KEY isn't set: add it in this environment's settings" >&2
		exit 1
	}
	"$bin/glass-e2e" real-model-policy anthropic "${GLASS_AGENTS_MODEL_ID:-claude-opus-5-5}" >"$dev/real-policy.toml"
	(umask 077 && printf '[anthropic]\napi_key = "%s"\n' "$key" >"$dev/model-secrets.toml")
	export GLASS_DEV_POLICY=$dev/real-policy.toml GLASS_DEV_MODEL_SECRETS=$dev/model-secrets.toml
	;;
scripted) export GLASS_DEV_SCRIPTED_MODEL=1 ;;
*)
	echo "agents-run: GLASS_AGENTS_MODEL is anthropic or scripted" >&2
	exit 1
	;;
esac

"$repo/tools/dev/host-run.sh" >"$dev/glassd.log" 2>&1 &
glassd=$!
front=""
cleanup() {
	kill "$glassd" $front 2>/dev/null || true
	rm -f "$dev/model-secrets.toml"
}
trap cleanup EXIT
# Stopped (Ctrl-C, kill): glassd and the front stop too, and the key goes.
trap 'exit 143' TERM INT HUP
for _ in $(seq 1 300); do
	[ -S "$run/control.sock" ] && break
	kill -0 "$glassd" 2>/dev/null || {
		echo "agents-run: glassd stopped; its log:" >&2
		tail -20 "$dev/glassd.log" >&2
		exit 1
	}
	sleep 0.1
done

token_file=()
if [ -n "${GLASS_AGENTS_TOKEN_FILE:-}" ]; then
	token_file=(--token-file "$GLASS_AGENTS_TOKEN_FILE")
fi
"${as_front[@]}" "$bin/glass-agents" --listen "$listen" --control-socket "$run/control.sock" \
	--web "$repo/apps/agents/dist" --name "${GLASS_AGENTS_NAME:-Dana Alvarez}" \
	--identity "$identity" "${token_file[@]}" &
front=$!
wait "$front"
