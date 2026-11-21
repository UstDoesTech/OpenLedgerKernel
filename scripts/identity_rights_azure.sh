#!/usr/bin/env bash
# Makes the custom role that lets the infra-apply identity make the rotation identity, and gives it
# to that identity with a condition that limits which roles it can hand out (decision D-18).
# A person runs it once, signed in to Azure. Usage: identity_rights_azure.sh <subscription id> <apply identity object id> [--apply]
set -euo pipefail
sub="${1:?subscription id}"; who="${2:?apply identity object id}"
here="$(cd "$(dirname "$0")/.." && pwd)"
role="$(python3 - "$here/infra/applier-rights.json" "$sub" <<'PY'
import json, sys
r = json.load(open(sys.argv[1]))["azure"]
print(json.dumps({"Name": r["custom_role"], "IsCustom": True, "Description": "Makes the rotation identity and its role assignments on the vault only.",
                  "Actions": r["actions"], "NotActions": [], "AssignableScopes": [f"/subscriptions/{sys.argv[2]}"]}, indent=2))
PY
)"
if [[ "${3:-}" != "--apply" ]]; then echo "$role"; exit 0; fi
tmp="$(mktemp)"; trap 'rm -f "$tmp"' EXIT; echo "$role" > "$tmp"
az role definition create --role-definition "$tmp" >/dev/null
az role assignment create --assignee-object-id "$who" --assignee-principal-type ServicePrincipal \
  --role "$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["azure"]["custom_role"])' "$here/infra/applier-rights.json")" \
  --scope "/subscriptions/$sub" >/dev/null
echo "done: one custom role and one assignment. Then remove User Access Administrator from the apply identity."
