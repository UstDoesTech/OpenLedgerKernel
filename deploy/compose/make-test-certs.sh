#!/usr/bin/env bash
# Makes throwaway certificates for trying the compose file out. They last two days and the CA
# key stays here. Never use them for real data: customers bring certificates from their own
# authority (see docs/self-hosted-postgres.md for what the names must be).
set -euo pipefail
cd "$(dirname "$0")"
rm -rf tls
mkdir -p tls
cd tls

openssl req -x509 -newkey rsa:2048 -nodes -keyout ca.key -out ca.crt -subj "/CN=ledger-example-ca" -days 2 2>/dev/null

issue() { # name common-name subject-alternative-name
  openssl req -newkey rsa:2048 -nodes -keyout "$1.key" -out "$1.csr" -subj "/CN=$2" 2>/dev/null
  printf 'subjectAltName=%s\n' "$3" > "$1.ext"
  openssl x509 -req -in "$1.csr" -CA ca.crt -CAkey ca.key -CAcreateserial -out "$1.crt" -days 2 -extfile "$1.ext" 2>/dev/null
}

# The client certificate that standbys present. Its name must be "replicator".
issue replication replicator DNS:replicator

for node in pg1 pg2 pg3; do
  issue "server-$node" "$node" "DNS:$node"
  mkdir -p "$node"
  cp "server-$node.crt" "$node/server.crt"
  cp "server-$node.key" "$node/server.key"
  cp ca.crt replication.crt replication.key "$node/"
done

# The nodes run as another user and must be able to read what is mounted. Real deployments
# give the files to that user (uid 999) instead of opening them to everyone.
chmod -R a+rX .
chmod a+r ./*/*.key
echo "Test certificates are in $(pwd). They are for trying it out only."
