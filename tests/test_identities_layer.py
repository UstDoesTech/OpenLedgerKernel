"""K-19c: the rotation identities, checked as text over the OpenTofu, without a cloud account.

infra/rotation-identity.json is the one list of what the identities may do. The modules read it, the
documents quote it, and these tests hold the HCL and the documents to it.
"""
import json
import re
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent
INFRA = ROOT / "infra"
LAYER = INFRA / "envs" / "europe" / "identities"
MODULES = INFRA / "modules" / "rotation-identity"
PERMISSIONS = json.loads((INFRA / "rotation-identity.json").read_text())
DOC = (ROOT / "docs" / "credentialed-workflows.md").read_text()


def hcl(path: Path) -> str:
    files = sorted(path.glob("*.tf"))
    return "\n".join(
        "\n".join(l for l in f.read_text().splitlines() if not l.lstrip().startswith("#"))
        for f in files
    )


def all_actions(aws: dict) -> set:
    return {a for key, v in aws.items() if isinstance(v, list) for a in v}


def test_the_layer_and_the_three_modules_exist():
    assert (LAYER / "main.tf").is_file()
    for cloud in ("aws", "azure", "gcp"):
        assert (MODULES / cloud / "main.tf").is_file()


def test_the_layer_uses_the_three_modules_and_the_regions_module():
    src = hcl(LAYER)
    for cloud in ("aws", "azure", "gcp"):
        assert f'source = "../../../modules/rotation-identity/{cloud}"' in src
    assert 'source = "../../../modules/regions"' in src


# ---- trust: only this repository's rotate-credentials environment -------------------------------

def test_the_permissions_file_names_this_repository_and_environment():
    assert PERMISSIONS["repository"] == "UstDoesTech/OpenLedgerKernel"
    assert PERMISSIONS["environment"] == "rotate-credentials"


@pytest.mark.parametrize("cloud", ["aws", "azure"])
def test_the_subject_is_the_repository_and_the_one_environment(cloud):
    src = hcl(MODULES / cloud)
    subjects = re.findall(r"subject\s*=\s*(.+)|:sub\"\s*=\s*(.+)", src)
    flat = [s for pair in subjects for s in pair if s]
    assert flat, "no subject condition"
    for s in flat:
        assert s.strip() == '"${var.subject_prefix}:environment:${var.environment}"', s
    assert "StringLike" not in src and "*" not in "".join(flat)


def test_the_layer_builds_the_prefix_from_the_repository_in_the_permissions_file():
    src = hcl(LAYER)
    assert 'repo:${local.identity.repository}' in src
    assert "ref:" not in src and "pull_request" not in src


def test_gcp_trusts_the_environment_attribute_and_nothing_wider():
    src = hcl(MODULES / "gcp")
    members = re.findall(r'member\s*=\s*"(principalSet[^"]+)"', src)
    assert len(members) == 1
    assert members[0].endswith("/attribute.environment/${var.environment}")
    assert "allUsers" not in src and "allAuthenticatedUsers" not in src


# ---- AWS: the listed actions and no more ---------------------------------------------------------

def aws_actions_in_hcl() -> set:
    return set(re.findall(r'"((?:kms|secretsmanager|s3|iam|sts|ssm|ec2):[A-Za-z*]+)"', hcl(MODULES / "aws")))


def test_the_aws_module_names_no_action_by_hand():
    # Every action comes from the permissions file, so the file is the only list.
    assert aws_actions_in_hcl() <= {"sts:AssumeRoleWithWebIdentity"}


def test_the_aws_actions_are_the_documented_ones_with_no_wildcard():
    actions = all_actions(PERMISSIONS["aws"])
    assert actions
    for a in actions:
        assert re.fullmatch(r"[a-z0-9]+:[A-Z][A-Za-z]+", a), f"{a} is a wildcard or malformed"
    assert actions == {
        "kms:GetPublicKey", "kms:Sign",
        "secretsmanager:GetSecretValue", "secretsmanager:ListSecretVersionIds", "secretsmanager:PutSecretValue",
        "kms:Decrypt", "kms:GenerateDataKey",
        "s3:GetObject", "s3:ListBucket", "s3:PutObject",
    }


FORBIDDEN_PREFIXES = ("iam:", "sts:", "organizations:", "account:", "ec2:", "lambda:", "ssm:")


def test_no_aws_action_changes_an_identity_or_a_permission():
    for a in all_actions(PERMISSIONS["aws"]):
        assert not a.startswith(FORBIDDEN_PREFIXES), a
    assert not {"kms:CreateGrant", "kms:PutKeyPolicy", "kms:ScheduleKeyDeletion", "kms:DisableKey",
                "secretsmanager:PutResourcePolicy", "secretsmanager:DeleteSecret",
                "s3:PutBucketPolicy", "s3:DeleteObject"} & all_actions(PERMISSIONS["aws"])


def test_every_aws_statement_names_its_resources_and_none_is_every_resource():
    src = hcl(MODULES / "aws")
    assert not re.search(r'Resource\s*=\s*"\*"', src)
    assert not re.search(r'Resource\s*=\s*\[\s*"\*"', src)
    assert '"arn:aws:s3:::${var.plan_store_bucket}/*"' in src  # a whole-bucket plan store, only when no prefix is given
    statements = re.findall(r"Resource\s*=\s*(.+)", src)
    assert len(statements) == 9  # sign, secrets, state, list state, logs, administrator, state key, logs key, secrets key
    for s in statements:
        assert "var." in s or "local." in s, s


def test_the_state_and_log_keys_get_one_action_each_on_one_key_each():
    aws = PERMISSIONS["aws"]
    assert aws["state_encryption"] == ["kms:Decrypt"]
    assert aws["plan_store_encryption"] == ["kms:GenerateDataKey"]
    src = hcl(MODULES / "aws")
    assert "Resource = [var.state_encryption_arn]" in src and "Resource = [var.plan_store_encryption_arn]" in src


def test_the_aws_role_has_the_same_document_as_its_permissions_boundary():
    src = hcl(MODULES / "aws")
    assert re.search(r"permissions_boundary\s*=\s*aws_iam_policy\.boundary\.arn", src)
    boundary = re.search(r'resource "aws_iam_policy" "boundary" \{(.*?)\n\}', src, re.S).group(1)
    inline = re.search(r'resource "aws_iam_role_policy" "this" \{(.*?)\n\}', src, re.S).group(1)
    assert "policy = local.policy" in re.sub(r"\s+", " ", boundary).replace("policy      =", "policy =")
    assert re.search(r"policy\s*=\s*local\.policy", inline)
    assert "managed_policy_arns" not in src and "aws_iam_role_policy_attachment" not in src


def test_the_aws_role_is_assumed_only_with_a_web_identity_and_the_audience_is_checked():
    src = hcl(MODULES / "aws")
    assert 'Action    = "sts:AssumeRoleWithWebIdentity"' in src
    assert 'token.actions.githubusercontent.com:aud" = "sts.amazonaws.com"' in src
    assert src.count("AssumeRole") == 1


def test_the_administrator_credential_is_read_only_when_proving():
    src = hcl(LAYER)
    assert 'administrator_secret_arn  = var.proving ?' in src
    assert 'var.proving ? ["${local.geo}/cells"]' in src
    assert 'administrator_secret_id = var.proving ?' in src
    assert set(PERMISSIONS["aws"]["administrator_secret"]) == {"secretsmanager:GetSecretValue"}


# ---- Azure: a managed identity with roles on the vault only --------------------------------------

def test_azure_uses_a_managed_identity_so_it_needs_no_right_in_entra_id():
    src = hcl(MODULES / "azure")
    assert 'resource "azurerm_user_assigned_identity"' in src
    assert 'resource "azurerm_federated_identity_credential"' in src
    assert "azuread_" not in src and "azuread" not in hcl(LAYER)
    assert 'audience            = ["api://AzureADTokenExchange"]' in src


def test_azure_roles_are_the_listed_ones_on_the_vault_and_never_on_the_subscription():
    src = hcl(MODULES / "azure")
    scopes = re.findall(r"^\s*scope\s*=\s*(.+)$", src, re.M)
    assert scopes and all(s.strip() in ("var.vault_id", "var.administrator_vault_id") for s in scopes), scopes
    assert not re.search(r"subscription|resource_group_id|management_group", " ".join(scopes))
    roles = set(PERMISSIONS["azure"]["vault_roles"]) | set(PERMISSIONS["azure"]["administrator_vault_roles"])
    assert roles == {"Key Vault Contributor", "Key Vault Secrets Officer", "Key Vault Secrets User"}
    for broad in ("Owner", "Contributor", "User Access Administrator", "Role Based Access Control Administrator"):
        assert broad not in roles
    assert not re.search(r'role_definition_name\s*=\s*"', src), "role names come from the permissions file"


# ---- GCP: roles on the secrets only --------------------------------------------------------------

def test_gcp_roles_are_the_listed_ones_and_only_on_secrets():
    src = hcl(MODULES / "gcp")
    assert "google_project_iam" not in src and "google_organization_iam" not in src and "google_folder_iam" not in src
    assert not re.search(r'resource "google_service_account_iam_(binding|policy)"', src)
    assert src.count('resource "google_secret_manager_secret_iam_member"') == 2
    roles = set(PERMISSIONS["gcp"]["secret_roles"]) | set(PERMISSIONS["gcp"]["administrator_secret_roles"])
    for r in roles:
        assert r.startswith("roles/secretmanager."), r
        assert not re.search(r"admin|owner|editor", r, re.I), r
    assert not re.search(r"serviceAccountKey|google_service_account_key", src)


def test_gcp_makes_no_key_so_no_secret_exists_to_leak():
    assert "google_service_account_key" not in hcl(MODULES / "gcp")
    assert "azuread_application_password" not in hcl(MODULES / "azure")
    assert "aws_iam_access_key" not in hcl(MODULES / "aws")


# ---- outputs: identifiers only -------------------------------------------------------------------

def test_the_layer_outputs_the_four_environment_secrets_and_no_secret():
    src = hcl(LAYER)
    outputs = re.search(r'output "environment_secrets" \{(.*?)\n\}', src, re.S).group(1)
    assert set(re.findall(r"^\s*([A-Z_]+)\s*=", outputs, re.M)) == {
        "AWS_ROLE_ARN", "AZURE_CLIENT_ID", "AZURE_TENANT_ID", "GCP_SERVICE_ACCOUNT"}
    assert not re.search(r"sensitive\s*=\s*true", src)
    assert not re.search(r"password|secret_value|private_key|access_key", src, re.I)


# ---- the document quotes the file ---------------------------------------------------------------

def test_the_document_lists_every_permission_in_the_file():
    for a in all_actions(PERMISSIONS["aws"]):
        assert f"`{a}`" in DOC, f"{a} is not in docs/credentialed-workflows.md"
    for r in PERMISSIONS["azure"]["vault_roles"] + PERMISSIONS["azure"]["administrator_vault_roles"]:
        assert r in DOC, r
    for r in PERMISSIONS["gcp"]["secret_roles"] + PERMISSIONS["gcp"]["administrator_secret_roles"]:
        assert f"`{r}`" in DOC, r


# ---- the applying identities' extra rights: the smallest set, bounded, and listed -----------------

APPLIER = json.loads((INFRA / "applier-rights.json").read_text())


def test_the_applier_aws_rights_name_resources_and_never_widen_or_leave_a_way_round():
    aws = APPLIER["aws"]
    granted = set(aws["role_actions"] + aws["policy_actions"] + aws["provider_actions"])
    for a in granted:
        assert re.fullmatch(r"iam:[A-Z][A-Za-z]+", a), a
    # It can make this one role and its boundary, and cannot attach any other policy to it, or to any role.
    assert not {"iam:AttachRolePolicy", "iam:DetachRolePolicy", "iam:PassRole", "iam:CreateUser",
                "iam:CreateAccessKey", "iam:PutUserPolicy", "iam:CreateOpenIDConnectProvider",
                "iam:UpdateOpenIDConnectProviderThumbprint", "iam:CreateLoginProfile"} & granted
    assert {"iam:DeleteRolePermissionsBoundary", "iam:AttachRolePolicy", "iam:DetachRolePolicy"} <= set(aws["denied_actions"])
    assert not set(aws["denied_actions"]) & granted
    script = (ROOT / "scripts" / "identity_rights_aws.sh").read_text()
    allows = re.findall(r'"Effect": "Allow".*?"Resource": (\[[^\]]*\])', script, re.S)
    assert len(allows) == 3 and all("*" not in a for a in allows), allows
    assert aws["role_name"] == PERMISSIONS["name"]
    assert f"ledger-{PERMISSIONS['name']}-boundary" == aws["boundary_name"]


def test_the_applier_azure_role_is_custom_and_has_no_wildcard_or_data_action():
    azure = APPLIER["azure"]
    assert azure["actions"]
    for a in azure["actions"]:
        assert "*" not in a, a
        assert a.startswith(("Microsoft.ManagedIdentity/", "Microsoft.Resources/subscriptions/resourceGroups/", "Microsoft.Authorization/roleAssignments/")), a
    assert not any("roleDefinitions" in a or "elevateAccess" in a or "policyAssignments" in a for a in azure["actions"])
    # The roles the identity can be given are the ones the permissions file lists, and no others.
    listed = set(PERMISSIONS["azure"]["vault_roles"]) | set(PERMISSIONS["azure"]["administrator_vault_roles"])
    assert set(azure["assignable_roles"]) == listed


def test_the_applier_gcp_role_makes_service_accounts_and_secret_policies_only():
    gcp = APPLIER["gcp"]
    perms = gcp["service_account_permissions"] + gcp["secret_permissions"]
    for p in perms:
        assert p.startswith(("iam.serviceAccounts.", "secretmanager.secrets.")), p
    assert "iam.serviceAccountKeys.create" not in perms and "iam.serviceAccounts.actAs" not in perms
    assert "iam.serviceAccounts.implicitDelegation" not in perms and "iam.serviceAccounts.getAccessToken" not in perms
    assert {"resourcemanager.projects.setIamPolicy", "resourcemanager.organizations.setIamPolicy"} <= set(gcp["denied_permissions"])
    assert gcp["service_account_prefix"] == f"{PERMISSIONS['name']}@"


@pytest.mark.parametrize("cloud", ["aws", "azure", "gcp"])
def test_the_rights_scripts_read_the_file_and_print_before_they_apply(cloud):
    script = (ROOT / "scripts" / f"identity_rights_{cloud}.sh").read_text()
    assert "applier-rights.json" in script
    assert "set -euo pipefail" in script
    assert '--apply' in script
    assert not re.search(r"AKIA|-----BEGIN|password|secret_key", script, re.I)


def test_the_document_lists_the_applier_rights_and_the_scripts():
    aws = APPLIER["aws"]
    for a in ("iam:CreateRole", "iam:UpdateRole", "iam:UpdateAssumeRolePolicy", "iam:PutRolePolicy",
              "iam:PutRolePermissionsBoundary", "iam:CreatePolicy", "iam:CreatePolicyVersion", "iam:GetOpenIDConnectProvider",
              "iam:DeleteRolePermissionsBoundary", "iam:AttachRolePolicy", "iam:DetachRolePolicy"):
        assert f"`{a}`" in DOC, a
    for name in (aws["role_name"], aws["boundary_name"], APPLIER["azure"]["custom_role"], APPLIER["gcp"]["custom_role"]):
        assert name in DOC, name
    for cloud in ("aws", "azure", "gcp"):
        assert f"scripts/identity_rights_{cloud}.sh" in DOC
