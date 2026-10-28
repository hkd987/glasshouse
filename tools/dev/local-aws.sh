#!/usr/bin/env bash
# AWS on this machine for Glasshouse Agents (deploy/local/README.md): moto
# for S3 and KMS, and stand-ins for the instance role, Marketplace metering
# and the load balancer's sign-in. In Docker when it runs; otherwise the
# same programs as processes (GLASS_LOCAL_AWS_PROCESSES=1 asks for that).
#
# usage: tools/dev/local-aws.sh up       # start, wait, write build/local-aws/env
#        tools/dev/local-aws.sh down     # stop (moto forgets everything)
#        tools/dev/local-aws.sh status
#
# build/local-aws/env is shell: `. build/local-aws/env` gives GLASS_LOCAL_*
# (endpoints, bucket, key ARN, product code, load balancer, signer).
# GLASS_LOCAL_AWS_PORT (default 7610) moves every port; GLASS_LOCAL_AWS_FRONT
# is where the load balancer sends people (default http://127.0.0.1:7500).
set -euo pipefail

repo=$(cd "$(dirname "$0")/../.." && pwd)
out="$repo/build/local-aws"
port="${GLASS_LOCAL_AWS_PORT:-7610}"
compose=(docker compose -f "$repo/deploy/local/compose.yaml")
# The image's moto, as processes.
moto_version=5.1.20
venv="$repo/build/tools/local-aws"

docker_runs() {
	[ -z "${GLASS_LOCAL_AWS_PROCESSES:-}" ] && command -v docker >/dev/null &&
		docker info >/dev/null 2>&1
}

wait_ready() {
	local url="http://127.0.0.1:$((port + 2))/_fake/ready"
	for _ in $(seq 1 240); do
		if curl -fsS "$url" >/dev/null 2>&1; then
			return 0
		fi
		sleep 0.25
	done
	echo "local-aws: not ready after a minute: $(curl -sS "$url" 2>&1 || true)" >&2
	return 1
}

write_env() {
	curl -fsS "http://127.0.0.1:$((port + 2))/_fake/config" | python3 -c '
import json, shlex, sys
c = json.load(sys.stdin)
names = {
    "region": "REGION", "s3_endpoint": "S3", "bucket": "BUCKET",
    "kms_key_arn": "KMS_KEY", "imds": "IMDS", "metering": "METERING",
    "product_code": "PRODUCT_CODE", "load_balancer": "LOAD_BALANCER",
    "public_keys": "PUBLIC_KEYS", "signer": "SIGNER",
}
print("# Written by tools/dev/local-aws.sh: AWS on this machine.")
print("GLASS_LOCAL_AWS=1")
for key, name in names.items():
    print(f"GLASS_LOCAL_{name}={shlex.quote(str(c[key]))}")
' >"$out/env"
}

up_processes() {
	if [ ! -x "$venv/bin/moto_server" ]; then
		python3 -m venv "$venv"
		"$venv/bin/pip" install --quiet --disable-pip-version-check "moto[server]==$moto_version"
	fi
	if [ ! -s "$out/moto.pid" ] || ! kill -0 "$(cat "$out/moto.pid")" 2>/dev/null; then
		"$venv/bin/moto_server" -H 127.0.0.1 -p "$port" >"$out/moto.log" 2>&1 &
		echo $! >"$out/moto.pid"
	fi
	if [ ! -s "$out/fake-aws.pid" ] || ! kill -0 "$(cat "$out/fake-aws.pid")" 2>/dev/null; then
		FAKE_AWS_PORT_BASE=$port FAKE_AWS_REGION="${GLASS_LOCAL_AWS_REGION:-us-east-1}" \
			FAKE_ALB_UPSTREAM="${GLASS_LOCAL_AWS_FRONT:-http://127.0.0.1:7500}" \
			"$venv/bin/python3" -I "$repo/deploy/local/fake-aws.py" >"$out/fake-aws.log" 2>&1 &
		echo $! >"$out/fake-aws.pid"
	fi
}

down_processes() {
	for name in fake-aws moto; do
		if [ -s "$out/$name.pid" ]; then
			kill "$(cat "$out/$name.pid")" 2>/dev/null || true
			rm -f "$out/$name.pid"
		fi
	done
}

mkdir -p "$out"
case "${1:-}" in
up)
	if docker_runs; then
		GLASS_LOCAL_AWS_PORT=$port "${compose[@]}" up -d --quiet-pull
		echo docker >"$out/how"
	else
		up_processes
		echo processes >"$out/how"
	fi
	wait_ready
	write_env
	echo "local-aws: up ($(cat "$out/how")); its settings are in $out/env"
	;;
down)
	if [ "$(cat "$out/how" 2>/dev/null)" = docker ]; then
		"${compose[@]}" down
	else
		down_processes
	fi
	rm -f "$out/env" "$out/how"
	;;
status)
	if curl -fsS "http://127.0.0.1:$((port + 2))/_fake/ready" 2>/dev/null; then
		echo
	else
		echo "local-aws: not running" >&2
		exit 1
	fi
	;;
*)
	sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'
	exit 2
	;;
esac
