#!/usr/bin/env bash
# Signs the machine in to its private Artifact Registry with the machine's own service account,
# using an access token from the metadata server. There is no stored credential.
set -euo pipefail

registry="$1"

token=$(curl -fsS -H 'Metadata-Flavor: Google' \
  http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token \
  | jq -r .access_token)

printf '%s' "$token" | docker login --username oauth2accesstoken --password-stdin "$registry"
