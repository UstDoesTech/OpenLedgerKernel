#!/usr/bin/env bash
# Reads a JSON object from standard input, with one entry for each certificate or key file, and
# writes the named files to /etc/ledger/tls. A file that is missing from the object stops the
# start, so a node never runs on half its certificates. Keys are readable by the container's
# postgres user (999) and by no one else.
set -euo pipefail

umask 077
target=/etc/ledger/tls
bundle=$(cat)
mkdir -p "$target"

for name in "$@"; do
  if ! jq -e --arg n "$name" 'has($n) and (.[$n] | type == "string" and length > 0)' <<< "$bundle" > /dev/null; then
    echo "the certificate secret has no $name" >&2
    exit 1
  fi
done

for name in "$@"; do
  jq -r --arg n "$name" '.[$n]' <<< "$bundle" > "$target/$name.new"
  case "$name" in
    *.key) chmod 0400 "$target/$name.new" ;;
    *) chmod 0444 "$target/$name.new" ;;
  esac
  chown 999:999 "$target/$name.new"
  mv "$target/$name.new" "$target/$name"
done
