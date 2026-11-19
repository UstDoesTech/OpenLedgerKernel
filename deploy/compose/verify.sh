#!/usr/bin/env bash
# Checks a running compose deployment: a quorum of two standbys, TLS, and a commit that reaches
# the standbys. Safe to run again. It makes one small table, verify_t.
set -euo pipefail
cd "$(dirname "$0")"
compose=(docker compose -f compose.yaml)

secret_id=$(sed -n 's/.*"secret_id": *"\([^"]*\)".*/\1/p' config/pg1.json)
secret_file=$(printf '%s' "$secret_id" | sed 's/[^A-Za-z0-9._-]/_/g')

password=
for _ in $(seq 1 90); do
  if password=$("${compose[@]}" exec -T pg1 cat "/secrets/out/$secret_file" 2>/dev/null) && [ -n "$password" ]; then
    break
  fi
  sleep 2
done
[ -n "$password" ] || { echo "the administrator's credential never appeared" >&2; exit 1; }

query() { # node sql
  "${compose[@]}" exec -T -e PGPASSWORD="$password" "$1" \
    psql -X -At -h /run/ledger -U ledger_admin -d postgres -c "$2"
}

quorum=
for _ in $(seq 1 90); do
  quorum=$(query pg1 "select count(*) from pg_stat_replication where sync_state = 'quorum'" 2>/dev/null || true)
  [ "$quorum" = 2 ] && break
  sleep 2
done
[ "$quorum" = 2 ] || { echo "expected two standbys in the quorum, found '${quorum:-none}'" >&2; exit 1; }

tls=$(query pg1 "select bool_and(ssl and version = 'TLSv1.3') from pg_stat_ssl s join pg_stat_replication r using (pid)")
[ "$tls" = t ] || { echo "replication is not using TLS 1.3" >&2; exit 1; }

slots=$(query pg1 "select count(*) from pg_replication_slots")
[ "$slots" = 0 ] || { echo "expected no replication slots, found $slots" >&2; exit 1; }

query pg1 "create table if not exists verify_t (i int)" > /dev/null
query pg1 "insert into verify_t values (1)" > /dev/null
rows=$(query pg1 "select count(*) from verify_t")
for node in pg2 pg3; do
  for _ in $(seq 1 30); do
    [ "$(query "$node" "select count(*) from verify_t" 2>/dev/null || true)" = "$rows" ] && continue 2
    sleep 1
  done
  echo "$node has not caught up" >&2
  exit 1
done

echo "ok: two standbys in the quorum over TLS 1.3, no slots, and a commit reached both standbys"
