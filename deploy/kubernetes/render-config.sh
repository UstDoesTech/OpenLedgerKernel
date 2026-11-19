#!/usr/bin/env bash
# Writes this pod's node configuration, which the node agent reads.
#
# Every pod runs this first. Which pod is the primary is a setting (PRIMARY_ORDINAL), not an
# accident of numbering, so that after a failover the old primary is told it is a standby.
# The agent then refuses to start it from its old primary data. That refusal is what stops
# two primaries running at once.
set -euo pipefail

out="${LEDGER_CONFIG_OUT:-/etc/ledger/node.json}"
base="${POD_NAME%-*}"
ordinal="${POD_NAME##*-}"
primary_ordinal="${PRIMARY_ORDINAL:-0}"

case "$primary_ordinal" in
  0 | 1 | 2) ;;
  *)
    echo "PRIMARY_ORDINAL must be 0, 1 or 2, not '$primary_ordinal'" >&2
    exit 1
    ;;
esac

standbys=()
for i in 0 1 2; do
  if [ "$i" != "$primary_ordinal" ]; then
    standbys+=("${base}-${i}")
  fi
done

if [ "$ordinal" = "$primary_ordinal" ]; then
  role=primary
  sync="ANY 1 (${standbys[0]}, ${standbys[1]})"
else
  role=standby
  sync=""
fi

# The primary's name inside the cluster. Its certificate must name this address.
address="${base}-${primary_ordinal}.${SERVICE_NAME}.${POD_NAMESPACE}.svc"

cat > "$out" <<JSON
{
  "tenant": "${TENANT}",
  "node": "${POD_NAME}",
  "role": "${role}",
  "postgres_version": 16,
  "primary_address": "${address}",
  "synchronous_standby_names": "${sync}",
  "secret_id": "${SECRET_ID}"
}
JSON
