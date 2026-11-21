#!/usr/bin/env bash
# Reads a JSON object from standard input, with one entry for each secret file, and writes the
# named files to /etc/ledger-api/secrets. A file that is missing from the object stops the start,
# so the API never runs on half its secrets. A name given as "optional:<name>" may be missing: the
# file is written when the entry is there and removed when it is not, so the directory always
# matches the secret. Every file is readable by the container's user (10001) and by no one else.
# LEDGER_API_SECRETS_DIR and LEDGER_API_SECRETS_OWNER exist so the tests can run this as anyone.
set -euo pipefail

umask 077
target="${LEDGER_API_SECRETS_DIR:-/etc/ledger-api/secrets}"
owner="${LEDGER_API_SECRETS_OWNER:-10001:10001}"
bundle=$(cat)
mkdir -p "$target"

has() {
  jq -e --arg n "$1" 'has($n) and (.[$n] | type == "string" and length > 0)' <<< "$bundle" > /dev/null
}

# Check every name before any file is touched.
for arg in "$@"; do
  name="${arg#optional:}"
  case "$name" in
    */* | .*) echo "not a file name: $name" >&2; exit 1 ;;
  esac
  if [ "$name" = "$arg" ] && ! has "$name"; then
    echo "the API secret has no $name" >&2
    exit 1
  fi
done

for arg in "$@"; do
  name="${arg#optional:}"
  if [ "$name" != "$arg" ] && ! has "$name"; then
    rm -f "$target/$name"
    continue
  fi
  jq -r --arg n "$name" '.[$n]' <<< "$bundle" > "$target/$name.new"
  chmod 0400 "$target/$name.new"
  chown "$owner" "$target/$name.new"
  mv "$target/$name.new" "$target/$name"
done
