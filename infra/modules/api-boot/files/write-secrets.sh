#!/usr/bin/env bash
# Reads a JSON object from standard input, with one entry for each secret file, and writes the
# named files to /etc/ledger-api/secrets. A file that is missing from the object stops the start,
# so the API never runs on half its secrets. Every file is readable by the container's user
# (10001) and by no one else.
set -euo pipefail

umask 077
target=/etc/ledger-api/secrets
bundle=$(cat)
mkdir -p "$target"

for name in "$@"; do
  case "$name" in
    */* | .*) echo "not a file name: $name" >&2; exit 1 ;;
  esac
  if ! jq -e --arg n "$name" 'has($n) and (.[$n] | type == "string" and length > 0)' <<< "$bundle" > /dev/null; then
    echo "the API secret has no $name" >&2
    exit 1
  fi
done

for name in "$@"; do
  jq -r --arg n "$name" '.[$n]' <<< "$bundle" > "$target/$name.new"
  chmod 0400 "$target/$name.new"
  chown 10001:10001 "$target/$name.new"
  mv "$target/$name.new" "$target/$name"
done
