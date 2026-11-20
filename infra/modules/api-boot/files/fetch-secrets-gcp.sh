#!/usr/bin/env bash
# Reads this cell's API secret from Secret Manager with the machine's own service account, and
# writes the files. There is no stored credential. The service account may read this one secret
# and no other. The access takes a short while to apply, so the read is tried again for about two
# minutes.
set -euo pipefail

secret="$1"
shift

for attempt in $(seq 1 24); do
  token=$(curl -fsS -H 'Metadata-Flavor: Google' \
    http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token \
    | jq -r .access_token) || token=
  if [ -n "$token" ]; then
    answer=$(curl -fsS -H "Authorization: Bearer $token" \
      "https://secretmanager.googleapis.com/v1/$secret/versions/latest:access") && {
      jq -r .payload.data <<< "$answer" | base64 -d | /opt/ledger-api/write-secrets.sh "$@"
      exit 0
    }
  fi
  echo "the API secret is not readable yet (attempt $attempt)" >&2
  sleep 5
done
exit 1
