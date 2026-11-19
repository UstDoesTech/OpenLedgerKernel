#!/usr/bin/env bash
# Proves the conformance suite can fail. Each mutation breaks one guarantee in a migration, runs
# crates/conformance/tests/active_active.rs against the three local nodes, and expects it to
# fail. A mutation the suite does not notice is a hole in the suite, so the script exits 1.
# The migration files are restored afterwards, even on Ctrl-C.
#
#   scripts/local_nodes.sh start && scripts/mutation_check.sh
#
# MUTATION_RUNS (default 3) repeats a run, because a missing row lock shows up as a race.
set -uo pipefail
cd "$(dirname "$0")/.."
M=crates/ledger-store/migrations
RUNS="${MUTATION_RUNS:-3}"
export RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-}"
[ -n "$RUSTUP_TOOLCHAIN" ] || unset RUSTUP_TOOLCHAIN

if ! git diff --quiet -- "$M"; then
  echo "refusing to run: $M has uncommitted changes that the restore would discard" >&2
  exit 2
fi
trap 'git checkout -q -- "$M"' EXIT

caught=0
missed=0

# mutate NAME FILE 'exact text to replace' 'replacement'
mutate() {
  local name="$1" file="$2" from="$3" to="$4"
  python3 - "$file" "$from" "$to" "$name" <<'PY' || exit 2
import sys
path, a, b, name = sys.argv[1:]
s = open(path).read()
n = s.count(a)
if n == 0:
    sys.exit(f"ERROR: mutation '{name}' no longer matches {path}, so the migration changed; update this script")
open(path, "w").write(s.replace(a, b))
PY
  local failed=no
  for i in $(seq 1 "$RUNS"); do
    if ! cargo test -q -p conformance --test active_active -- --ignored >/tmp/mutation.$$.log 2>&1; then
      failed=yes
      break
    fi
  done
  git checkout -q -- "$M"
  if [ "$failed" = yes ]; then
    echo "CAUGHT  $name: $(sed -n '/^failures:$/,/^test result/p' /tmp/mutation.$$.log | grep -E '^    [a-z_0-9]+$' | tr -d ' ' | sort -u | paste -sd, -)"
    caught=$((caught + 1))
  else
    echo "MISSED  $name: the suite still passed with this guarantee removed"
    missed=$((missed + 1))
  fi
}

echo "baseline: the suite must pass first"
cargo test -q -p conformance --test active_active -- --ignored >/tmp/mutation.$$.log 2>&1 || {
  tail -20 /tmp/mutation.$$.log
  echo "the unmutated suite fails, so a mutation proves nothing" >&2
  exit 2
}

# post_leg and the hold functions were last defined in migration 4, so the live copy is there.
mutate "fencing: a stale epoch is accepted on write" "$M/0004_holds.sql" \
  "if p_epoch is distinct from latest then" "if false and p_epoch is distinct from latest then"
mutate "fencing: the sweep never quarantines an entry a fence voids" "$M/0005_replication.sql" \
  "as \$\$
begin
  with void as (" "as \$\$
begin
  return;
  with void as ("
mutate "row lock: post_leg reads the account without locking it" "$M/0004_holds.sql" \
   "   where a.tenant_id = p_tenant::tenant_ref and a.id = p_account::account_ref
   for update;" "   where a.tenant_id = p_tenant::tenant_ref and a.id = p_account::account_ref;"

rm -f /tmp/mutation.$$.log
echo "caught $caught, missed $missed"
[ "$missed" = 0 ]
