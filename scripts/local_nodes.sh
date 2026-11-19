#!/usr/bin/env bash
# Start/stop three throwaway local Postgres nodes (ports 5431-5433) that stand in for three
# regions. Needs the Postgres server binaries (e.g. apt install postgresql-16).
#   scripts/local_nodes.sh start | stop | status
set -euo pipefail

BIN="${PG_BIN:-$(ls -d /usr/lib/postgresql/*/bin 2>/dev/null | sort -V | tail -1)}"
DIR="${LEDGER_LAB_DIR:-$HOME/.ledger-lab}"
BASE_PORT="${LEDGER_LAB_PORT:-5430}"

as_pg() {  # Postgres refuses to run as root
  if [ "$(id -u)" = 0 ]; then su postgres -c "$*"; else bash -c "$*"; fi
}

case "${1:-}" in
  start)
    mkdir -p "$DIR"; [ "$(id -u)" = 0 ] && chown postgres "$DIR"
    for i in 1 2 3; do
      d="$DIR/node$i"
      [ -d "$d" ] || as_pg "$BIN/initdb -D $d -A trust -U ledger >/dev/null"
      as_pg "$BIN/pg_ctl -D $d -o '-p $((BASE_PORT + i)) -k /tmp -c listen_addresses=127.0.0.1 -c fsync=off' -l $d.log -w start >/dev/null" || true
    done
    echo "nodes on 127.0.0.1:$((BASE_PORT + 1))-$((BASE_PORT + 3)) (user ledger, db postgres)" ;;
  stop)
    for i in 1 2 3; do as_pg "$BIN/pg_ctl -D $DIR/node$i -m fast stop >/dev/null" || true; done ;;
  status)
    for i in 1 2 3; do as_pg "$BIN/pg_ctl -D $DIR/node$i status" | head -1 || true; done ;;
  *) echo "usage: $0 start|stop|status"; exit 2 ;;
esac
