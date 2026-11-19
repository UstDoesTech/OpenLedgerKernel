#!/usr/bin/env bash
# Signs the machine in to its private ECR registry with the machine's own role. There is no
# stored credential: the role's short-lived keys come from the instance metadata service
# (version 2) and the request to ECR is signed by curl.
set -euo pipefail

region="$1"
registry="$2"
imds=http://169.254.169.254

token=$(curl -fsS -X PUT "$imds/latest/api/token" -H 'X-aws-ec2-metadata-token-ttl-seconds: 60')
role=$(curl -fsS -H "X-aws-ec2-metadata-token: $token" "$imds/latest/meta-data/iam/security-credentials/" | head -n 1)
keys=$(curl -fsS -H "X-aws-ec2-metadata-token: $token" "$imds/latest/meta-data/iam/security-credentials/$role")

access_key=$(jq -r .AccessKeyId <<< "$keys")
secret_key=$(jq -r .SecretAccessKey <<< "$keys")
session=$(jq -r .Token <<< "$keys")

answer=$(curl -fsS "https://api.ecr.$region.amazonaws.com/" \
  --aws-sigv4 "aws:amz:$region:ecr" --user "$access_key:$secret_key" \
  -H "x-amz-security-token: $session" \
  -H 'Content-Type: application/x-amz-json-1.1' \
  -H 'X-Amz-Target: AmazonEC2ContainerRegistry_V20150921.GetAuthorizationToken' \
  -d '{}')

# The token is "AWS:<password>" in base64.
jq -r '.authorizationData[0].authorizationToken' <<< "$answer" | base64 -d | cut -d: -f2- \
  | docker login --username AWS --password-stdin "$registry"
