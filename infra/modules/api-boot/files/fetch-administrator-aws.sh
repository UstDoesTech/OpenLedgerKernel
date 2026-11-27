#!/usr/bin/env bash
# Reads the managed database's administrator secret from AWS Secrets Manager with the machine's own
# role, and writes one connection string for the one-off role step to a file in memory
# (/run/ledger-administrator, mode 0600). The host, port and database come from the cell's db.conn,
# which holds no password. The file is never in the API's secret and is removed when the step ends.
# This is run only when a person has switched the step on (K-24cc).
set -euo pipefail

secret="$1"
region=$(cut -d: -f4 <<< "$secret")
imds=http://169.254.169.254
dir=/run/ledger-administrator

umask 077
mkdir -p "$dir"
conn=$(cat /etc/ledger-api/secrets/db.conn)
field() { tr ' ' '\n' <<< "$conn" | sed -n "s/^$1=//p" | head -n 1; }
host=$(field host)
port=$(field port)
database=$(field dbname)
[ -n "$host" ] && [ -n "$port" ] && [ -n "$database" ] || { echo "db.conn does not name a host, a port and a database" >&2; exit 1; }

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
        held=$(jq -r .SecretString <<< "$answer")
        user=$(jq -r .username <<< "$held")
        password=$(jq -r .password <<< "$held")
        [ -n "$user" ] && [ "$user" != null ] && [ -n "$password" ] && [ "$password" != null ] || { echo "the administrator's secret does not hold a user and a password" >&2; exit 1; }
        password=${password//\\/\\\\}
        password=${password//\'/\\\'}
        # A here-document, so nothing is printed and the password is on no command line.
        cat > "$dir/admin.conn.new" <<CONNECTION
host=$host port=$port user=$user password='$password' dbname=$database sslmode=require
CONNECTION
        mv "$dir/admin.conn.new" "$dir/admin.conn"
        exit 0
      }
    fi
  fi
  echo "the administrator's secret is not readable yet (attempt $attempt)" >&2
  sleep 5
done
exit 1
