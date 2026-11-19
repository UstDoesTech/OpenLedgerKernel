#!/usr/bin/env bash
# Run a command with every line of its output sent to a log file, never to the job's log.
#
# This repository is public, so anyone can read a workflow's log. The output of tofu and of the
# cloud tools holds account, subscription and resource identifiers. The log files are kept in the
# private plan store (scripts/keep_logs.sh), which only the run's cloud identity can write and
# only a person with cloud access can read.
#
# Usage: LOG_DIR=<dir> scripts/quiet.sh <log name> <command> [argument...]
# On failure it prints the exit code and where the log is, and nothing the command said.
set -uo pipefail

name="${1:?usage: quiet.sh <log name> <command> [argument...]}"
shift
[[ "$name" =~ ^[a-z0-9-]+$ ]] || { echo "::error::a log name is lower case letters, digits and hyphens"; exit 2; }
dir="${LOG_DIR:?LOG_DIR is not set}"
mkdir -p "$dir"

"$@" > "$dir/$name.log" 2>&1
rc=$?
if [ "$rc" -ne 0 ]; then
  echo "::error::$name failed with exit code $rc. The log is in the plan store under this run's id, not here."
fi
exit "$rc"
