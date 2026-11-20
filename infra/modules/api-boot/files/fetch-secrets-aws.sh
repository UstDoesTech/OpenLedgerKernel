#!/usr/bin/env bash
# Reads this cell's API secret from AWS Secrets Manager with the machine's own role, and writes
# the files. There is no stored credential: the role's short-lived keys come from the instance
# metadata service (version 2) and curl signs the request. The role may read this one secret and
# no other. The secret takes a short while to be readable after a machine is made, so the read
# is tried again for about two minutes.
set -euo pipefail

secret="$1"
shift
region=$(cut -d: -f4 <<< "$secret")
imds=http://169.254.169.254

for attempt in $(seq 1 24); do
  token=$(curl -fsS -X PUT "$imds/latest/api/token" -H 'X-aws-ec2-metadata-token-ttl-seconds: 60') || token=
  if [ -n "$token" ]; then
    role=$(curl -fsS -H "X-aws-ec2-metadata-token: $token" "$imds/latest/meta-data/iam/security-credentials/" | head -n 1) || role=
    keys=$(curl -fsS -H "X-aws-ec2-metadata-token: $token" "$imds/latest/meta-data/iam/security-credentials/$role") || keys=
    if [ -n "$keys" ]; then
      answer=$(curl -fsS "https://secretsmanager.$region.amazonaws.com/" \
        --aws-sigv4 "aws:amz:$region:secretsmanager" \
        --user "$(jq -r .AccessKeyId <<< "$keys"):$(jq -r .SecretAccessKey <<< "$keys")" \
        -H "x-amz-security-token: $(jq -r .Token <<< "$keys")" \
        -H 'Content-Type: application/x-amz-json-1.1' \
        -H 'X-Amz-Target: secretsmanager.GetSecretValue' \
        -d "$(jq -n --arg id "$secret" '{SecretId: $id}')") && {
        jq -r .SecretString <<< "$answer" | /opt/ledger-api/write-secrets.sh "$@"
        exit 0
      }
    fi
  fi
  echo "the API secret is not readable yet (attempt $attempt)" >&2
  sleep 5
done
exit 1
