#!/usr/bin/env bash
# The smallest proof that the write path works on a running environment: open two accounts, make a
# transfer, make the same transfer again, read both balances back.
#
# This repository is public, so the script prints counts only. Never an identifier, a token, a
# balance or an address. It prints "ok" or "failed" for each step, and a total.
#
# Usage:
#   LEDGER_API_URL=https://<api address> LEDGER_API_TOKEN_FILE=<file> \
#     [LEDGER_API_CA_FILE=<file>] [LEDGER_API_URL_PAYEE=https://<another cell's api address>] \
#     scripts/smoke_write.sh
#
# With LEDGER_API_URL_PAYEE the payee is opened at that cell, so the transfer crosses cells.
# The token is read from a file, never from the command line, so it does not show in a process
# list. Use a test tenant. The proving environment holds no real money and no customer data.
set -uo pipefail

url="${LEDGER_API_URL:?LEDGER_API_URL is not set}"
payee_url="${LEDGER_API_URL_PAYEE:-$url}"
token_file="${LEDGER_API_TOKEN_FILE:?LEDGER_API_TOKEN_FILE is not set}"
[ -r "$token_file" ] || { echo "LEDGER_API_TOKEN_FILE cannot be read"; exit 2; }
command -v curl > /dev/null && command -v jq > /dev/null || { echo "curl and jq are needed"; exit 2; }

umask 077
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# The token goes to curl in a header file, so it is never an argument.
printf 'Authorization: Bearer %s\n' "$(tr -d '\n' < "$token_file")" > "$work/auth"

curl_args=(--silent --max-time 20 --header "@$work/auth" --header 'Content-Type: application/json')
[ -n "${LEDGER_API_CA_FILE:-}" ] && curl_args+=(--cacert "$LEDGER_API_CA_FILE")

# call <name> <base url> <method> <path> [idempotency key] [body]
# The status goes to <name>.status and the body to <name>.json, both in the private work folder.
call() {
  local name="$1" base="$2" method="$3" path="$4" key="${5:-}" body="${6:-}"
  local args=("${curl_args[@]}" --request "$method" --output "$work/$name.json" --write-out '%{http_code}')
  [ -n "$key" ] && args+=(--header "Idempotency-Key: $key")
  [ -n "$body" ] && args+=(--data "$body")
  curl "${args[@]}" "${base%/}$path" > "$work/$name.status" 2> /dev/null || echo 000 > "$work/$name.status"
}

steps=0
failed=0
step() { # <label> <name> <expected status>
  steps=$((steps + 1))
  local got
  got=$(cat "$work/$2.status")
  if [ "$got" = "$3" ]; then
    echo "$1: ok"
  else
    echo "$1: failed (status class ${got:0:1}xx)"
    failed=$((failed + 1))
  fi
}
check() { # <label> <true|false>
  steps=$((steps + 1))
  if [ "$2" = true ]; then
    echo "$1: ok"
  else
    echo "$1: failed"
    failed=$((failed + 1))
  fi
}

run="smoke-$(date +%s)-$$"

call payer "$url" POST /v1/accounts "$run-payer" '{"currency":"GBP","overdraft_limit":{"minor":1000,"currency":"GBP"}}'
step "open the payer's account" payer 201
call payee "$payee_url" POST /v1/accounts "$run-payee" '{"currency":"GBP"}'
step "open the payee's account" payee 201

payer_id=$(jq -r '.account_id // empty' "$work/payer.json" 2> /dev/null)
payee_id=$(jq -r '.account_id // empty' "$work/payee.json" 2> /dev/null)
if [ -z "$payer_id" ] || [ -z "$payee_id" ]; then
  echo "no account to move value between; stopping"
  echo "steps: $steps, failed: $((failed + 1))"
  exit 1
fi

transfer=$(jq -n --arg a "$payer_id" --arg b "$payee_id" \
  '{payer_account_id: $a, payee_account_id: $b, amount: {minor: 100, currency: "GBP"}}')

call first "$url" POST /v1/transfers "$run-transfer" "$transfer"
step "make the transfer" first 201
call again "$url" POST /v1/transfers "$run-transfer" "$transfer"
step "repeat the transfer" again 201

same=false
[ "$(jq -r '.journal_id' "$work/first.json" 2> /dev/null)" = "$(jq -r '.journal_id' "$work/again.json" 2> /dev/null)" ] && same=true
replayed=$(jq -r '.replayed' "$work/again.json" 2> /dev/null)
check "the repeat returned the same journal and wrote nothing" "$([ "$same" = true ] && [ "$replayed" = true ] && echo true || echo false)"

call payer_balance "$url" GET "/v1/accounts/$payer_id/balances"
step "read the payer's balances" payer_balance 200
call payee_balance "$payee_url" GET "/v1/accounts/$payee_id/balances"
step "read the payee's balances" payee_balance 200

# The amounts are compared here and never printed. One transfer of 100, made once.
[ "$(jq -r '.posted.minor' "$work/payer_balance.json" 2> /dev/null)" = "-100" ] && payer_ok=true || payer_ok=false
[ "$(jq -r '.posted.minor' "$work/payee_balance.json" 2> /dev/null)" = "100" ] && payee_ok=true || payee_ok=false
check "the payer was debited exactly once" "$payer_ok"
check "the payee was credited exactly once" "$payee_ok"

echo "steps: $steps, failed: $failed"
[ "$failed" -eq 0 ]
