#!/usr/bin/env bash
# Signs the machine in to its private Azure Container Registry with the machine's own managed
# identity: an Azure Resource Manager token from the instance metadata service is exchanged
# for a registry refresh token. There is no stored credential.
set -euo pipefail

registry="$1"
tenant="$2"
imds=http://169.254.169.254

arm_token=$(curl -fsS -H Metadata:true \
  "$imds/metadata/identity/oauth2/token?api-version=2018-02-01&resource=https%3A%2F%2Fmanagement.azure.com%2F" \
  | jq -r .access_token)

refresh=$(curl -fsS "https://$registry/oauth2/exchange" \
  -d grant_type=access_token -d "service=$registry" -d "tenant=$tenant" \
  --data-urlencode "access_token=$arm_token" | jq -r .refresh_token)

printf '%s' "$refresh" | docker login --username 00000000-0000-0000-0000-000000000000 --password-stdin "$registry"
