#!/usr/bin/env bash
# Steps 4 to 6 of docs/runbooks/first-entry.md in one run: ask each cell's API for its health, make
# a first entry at the first cell, then make a transfer between the first cell and every other one.
#
# Run it from inside the network: on the VPN, or on a machine whose address is in the API layer's
# api_client_cidrs. The APIs have no public address.
#
# This repository is public, so it prints counts only: "ok" or "failed" for each step, with the cell
# as its number in the list, and a total. Never an address, a name, an identifier, a token or a
# balance. Use the proving environment and a test tenant.
#
# Usage:
#   LEDGER_APIS_FILE=<the api layer's `apis` output, as JSON> \
#   LEDGER_API_TOKEN_FILES="<file for cell 1> <file for cell 2> ..." \
#   LEDGER_API_CA_FILE=<file> scripts/first_entry_check.sh
#
# or, in place of the file, LEDGER_API_URLS="https://<cell 1> https://<cell 2> ...". Each cell has
# its own token, so the token files are in the same order as the cells: with the file, that is the
# order of its keys (aws, azure, gcp). Each file holds one token and is read by the script, never
# passed on a command line.
set -uo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
command -v curl > /dev/null && command -v jq > /dev/null || { echo "curl and jq are needed"; exit 2; }

if [ -n "${LEDGER_APIS_FILE:-}" ]; then
  [ -r "$LEDGER_APIS_FILE" ] || { echo "LEDGER_APIS_FILE cannot be read"; exit 2; }
  # shellcheck disable=SC2207
  urls=($(jq -r 'to_entries | sort_by(.key)[] | "https://\(.value.address):\(.value.port)"' "$LEDGER_APIS_FILE" 2> /dev/null))
else
  read -r -a urls <<< "${LEDGER_API_URLS:-}"
fi
read -r -a tokens <<< "${LEDGER_API_TOKEN_FILES:?LEDGER_API_TOKEN_FILES is not set}"

[ "${#urls[@]}" -ge 1 ] || { echo "no cell to check: set LEDGER_APIS_FILE or LEDGER_API_URLS"; exit 2; }
[ "${#urls[@]}" -eq "${#tokens[@]}" ] || { echo "there must be one token file for each cell: ${#urls[@]} cells, ${#tokens[@]} files"; exit 2; }
for f in "${tokens[@]}"; do [ -r "$f" ] || { echo "a token file cannot be read"; exit 2; }; done

curl_args=(--silent --max-time 20)
[ -n "${LEDGER_API_CA_FILE:-}" ] && curl_args+=(--cacert "$LEDGER_API_CA_FILE")

umask 077
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

steps=0
failed=0

# Step 4: health. Nothing is written until every cell says it is ready.
echo "step 4: each cell's health"
ready=true
for i in "${!urls[@]}"; do
  n=$((i + 1))
  steps=$((steps + 1))
  code=$(curl "${curl_args[@]}" --output "$work/health" --write-out '%{http_code}' "${urls[$i]%/}/healthz" 2> /dev/null) || code=000
  word=$(jq -r '.status // empty' "$work/health" 2> /dev/null)
  # The status is one of a short fixed list of words. Anything else is not repeated.
  case "$word" in ok | leases_missing | control_plane_unavailable | draining) ;; *) word=unknown ;; esac
  if [ "$code" = 200 ] && [ "$word" = ok ]; then
    echo "  cell $n: ok"
  else
    echo "  cell $n: failed (status class ${code:0:1}xx, $word)"
    failed=$((failed + 1))
    ready=false
  fi
done

if [ "$ready" != true ]; then
  echo "a cell is not ready, so nothing was written"
  echo "steps: $steps, failed: $failed"
  exit 1
fi

# The smoke script's own total is its last line. Its other lines are repeated with a prefix.
smoke() { # <label> <payer url> <payer token file> [<payee url> <payee token file>]
  local label="$1" out last s f
  out=$(
    export LEDGER_API_URL="$2" LEDGER_API_TOKEN_FILE="$3" LEDGER_API_CA_FILE="${LEDGER_API_CA_FILE:-}"
    if [ -n "${4:-}" ]; then
      export LEDGER_API_URL_PAYEE="$4" LEDGER_API_TOKEN_FILE_PAYEE="$5"
    fi
    bash "$here/smoke_write.sh" 2>&1
  )
  echo "$label"
  sed -e '$d' <<< "$out" | sed 's/^/  /'
  last=$(tail -n 1 <<< "$out")
  s=$(sed -n 's/^steps: \([0-9][0-9]*\), failed: \([0-9][0-9]*\)$/\1/p' <<< "$last")
  f=$(sed -n 's/^steps: \([0-9][0-9]*\), failed: \([0-9][0-9]*\)$/\2/p' <<< "$last")
  if [ -z "$s" ]; then
    echo "  the script did not finish"
    s=1
    f=1
  fi
  steps=$((steps + s))
  failed=$((failed + f))
}

# Step 5: a first entry at the first cell, with both accounts there.
smoke "step 5: a first entry at cell 1" "${urls[0]}" "${tokens[0]}"

# Step 6: the payer at the first cell and the payee at each other cell in turn.
if [ "${#urls[@]}" -lt 2 ]; then
  echo "step 6: needs two cells"
  steps=$((steps + 1))
  failed=$((failed + 1))
else
  for i in "${!urls[@]}"; do
    [ "$i" -eq 0 ] && continue
    smoke "step 6: a transfer from cell 1 to cell $((i + 1))" "${urls[0]}" "${tokens[0]}" "${urls[$i]}" "${tokens[$i]}"
  done
fi

echo "steps: $steps, failed: $failed"
[ "$failed" -eq 0 ]
