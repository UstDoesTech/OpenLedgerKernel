#!/usr/bin/env bash
# Makes the custom role that lets the infra-apply service account make the rotation service account and
# set the policy on the API secrets, with a deny policy for project-level policy changes (decision D-18).
# A person runs it once, signed in to Google Cloud. Usage: identity_rights_gcp.sh <project id> <apply service account email> [--apply]
set -euo pipefail
project="${1:?project id}"; apply_sa="${2:?apply service account email}"
here="$(cd "$(dirname "$0")/.." && pwd)"
read -r role prefix perms denied < <(python3 - "$here/infra/applier-rights.json" <<'PY'
import json, sys
r = json.load(open(sys.argv[1]))["gcp"]
print(r["custom_role"], r["service_account_prefix"], ",".join(r["service_account_permissions"] + r["secret_permissions"]), " ".join(r["denied_permissions"]).replace(" ", ","))
PY
)
if [[ "${3:-}" != "--apply" ]]; then
  echo "role $role in $project: $perms"
  echo "bound to $apply_sa where the resource is a service account whose name starts with $prefix, or a secret"
  echo "deny for $apply_sa: ${denied//,/ }"
  exit 0
fi
gcloud iam roles create "$role" --project "$project" --title "Ledger rotation identity manager" --permissions "$perms"
gcloud projects add-iam-policy-binding "$project" --member "serviceAccount:$apply_sa" --role "projects/$project/roles/$role" \
  --condition="title=rotation-identity-only,expression=resource.type == 'secretmanager.googleapis.com/Secret' || resource.name.extract('/serviceAccounts/{sa}').startsWith('${prefix}')"
echo "done: one custom role and one conditional binding. Add a deny policy for: ${denied//,/ } (gcloud iam policies create)"
