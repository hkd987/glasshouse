#!/bin/sh
# Usage: stripe-paid.sh <url> <session> <org> <cents> [webhook secret]
#
# Posts Stripe's checkout.session.completed for a paid model-credits
# checkout to <url>/stripe/webhook (the billing relay, ADR 0040, or a
# console that takes Stripe's webhooks itself), signed as Stripe signs
# webhooks: HMAC-SHA256 over "<time>.<body>" with the endpoint's secret.
# The secret defaults to the development one; pass another to see a
# forged event refused.
set -eu
url=$1 session=$2 org=$3 cents=$4 secret=${5:-whsec_dev_not_a_secret}
body=$(printf '{"id":"evt_%s","type":"checkout.session.completed","data":{"object":{"id":"%s","mode":"payment","payment_status":"paid","amount_total":%s,"currency":"usd","metadata":{"org":"%s","kind":"model_credits"}}}}' \
	"$session" "$session" "$cents" "$org")
t=$(date +%s)
sig=$(printf '%s.%s' "$t" "$body" | openssl dgst -sha256 -hmac "$secret" -hex | sed 's/.* //')
curl -s -o /dev/null -w "Stripe → relay: HTTP %{http_code}\n" \
	-H "Stripe-Signature: t=$t,v1=$sig" -H 'Content-Type: application/json' \
	--data "$body" "$url/stripe/webhook"
