#!/usr/bin/env bash
# Reads this node's certificate secret from Azure Key Vault with the machine's own managed
# identity, and writes the files. There is no stored credential. The identity may read this
# node's secret and no other. The role assignment takes a short while to apply, so the read is
# tried again for about two minutes.
set -euo pipefail

secret="$1"
shift
imds=http://169.254.169.254

for attempt in $(seq 1 24); do
  token=$(curl -fsS -H Metadata:true \
    "$imds/metadata/identity/oauth2/token?api-version=2018-02-01&resource=https%3A%2F%2Fvault.azure.net" \
    | jq -r .access_token) || token=
  if [ -n "$token" ]; then
    answer=$(curl -fsS -H "Authorization: Bearer $token" "$secret?api-version=7.4") && {
      jq -r .value <<< "$answer" | /opt/ledger/write-tls.sh "$@"
      exit 0
    }
  fi
  echo "the certificate secret is not readable yet (attempt $attempt)" >&2
  sleep 5
done
exit 1
