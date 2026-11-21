#!/usr/bin/env bash
# Gives the infra-apply role the rights to make the rotate-credentials role and its boundary, and
# nothing wider (decision D-18). A person runs it once, in their own AWS session.
# Usage: identity_rights_aws.sh <apply role name> [--apply]. Without --apply it prints the policy.
set -euo pipefail
role="${1:?the infra-apply role name}"; shift
here="$(cd "$(dirname "$0")/.." && pwd)"
account="$(aws sts get-caller-identity --query Account --output text)"
policy="$(python3 - "$here/infra/applier-rights.json" "$account" <<'PY'
import json, sys
r = json.load(open(sys.argv[1]))["aws"]; acct = sys.argv[2]
role_arn = f"arn:aws:iam::{acct}:role/{r['role_name']}"
pol_arn = f"arn:aws:iam::{acct}:policy/{r['boundary_name']}"
print(json.dumps({"Version": "2012-10-17", "Statement": [
    {"Sid": "RotationRole", "Effect": "Allow", "Action": r["role_actions"], "Resource": [role_arn]},
    {"Sid": "RotationBoundary", "Effect": "Allow", "Action": r["policy_actions"], "Resource": [pol_arn]},
    {"Sid": "ReadTheGitHubProvider", "Effect": "Allow", "Action": r["provider_actions"],
     "Resource": [f"arn:aws:iam::{acct}:oidc-provider/token.actions.githubusercontent.com"]},
    {"Sid": "NoOtherWay", "Effect": "Deny", "Action": r["denied_actions"], "Resource": ["*"]},
]}, indent=2))
PY
)"
if [[ "${1:-}" != "--apply" ]]; then echo "$policy"; exit 0; fi
aws iam put-role-policy --role-name "$role" --policy-name ledger-rotation-identity --policy-document "$policy"
echo "done: one inline policy on the apply role"
