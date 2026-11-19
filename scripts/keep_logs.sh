#!/usr/bin/env bash
# Copy a run's logs to the private plan store, under the run's id.
#
# Usage: PLAN_STORE=s3://<bucket>/<prefix> LOG_DIR=<dir> scripts/keep_logs.sh <stage>
# The store is a private bucket the run's identity may write to. Nothing is printed except
# whether the copy worked, because the bucket's name and the files' names are identifiers.
set -uo pipefail

stage="${1:?usage: keep_logs.sh <stage>}"
[[ "$stage" =~ ^[a-z0-9-]+$ ]] || { echo "::error::a stage is lower case letters, digits and hyphens"; exit 2; }
store="${PLAN_STORE:?PLAN_STORE is not set}"
dir="${LOG_DIR:?LOG_DIR is not set}"
run="${GITHUB_RUN_ID:?GITHUB_RUN_ID is not set}"
[[ "$store" =~ ^s3://[a-z0-9.-]+(/[A-Za-z0-9._/-]*)?$ ]] || { echo "::error::PLAN_STORE must be an s3:// address"; exit 2; }
[ -d "$dir" ] || { echo "No logs to keep."; exit 0; }

if aws s3 cp "$dir" "${store%/}/$run/$stage/" --recursive --only-show-errors > /dev/null 2>&1; then
  echo "Logs kept in the plan store under run $run, $stage."
else
  echo "::error::could not write the logs to the plan store"
  exit 1
fi
