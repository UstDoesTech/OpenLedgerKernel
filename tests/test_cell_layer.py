"""Rules for the first tenant's cell layer, checked without a cloud account."""
import re
from pathlib import Path

from test_envs import ENVS, blocks, text

ROOT = Path(__file__).resolve().parent.parent
LAYER = ENVS / "europe" / "cell-first"
POSTGRES = ROOT / "infra" / "modules" / "postgres" / "variables.tf"


def src():
    return text(LAYER)


def test_the_layer_is_for_one_tenant_whose_id_is_written_once():
    s = src()
    ids = set(re.findall(r"ten_[0-9a-z]+", s))
    assert ids == {"ten_bankofmumanddad"}, ids
    assert re.search(r'tenant\s*=\s*"ten_[0-9a-z]{1,24}"', s)
    assert len(re.findall(r'"ten_[0-9a-z]+"', s)) == 1


def test_it_makes_a_key_in_every_cloud_and_never_uses_the_control_planes_or_anyone_elses():
    s = src()
    for kind in ("aws_kms_key", "azurerm_key_vault_key", "google_kms_crypto_key"):
        assert re.search(rf'resource "{kind}" "cell"', s), kind
    # It reads no other layer's key and no other layer's identity.
    assert 'data "terraform_remote_state" "control_plane"' not in s
    assert "control_plane" not in s
    assert "enable_key_rotation     = true" in s and "auto_key_rotation_enabled = true" in s
    assert 'rotation_period = "7776000s"' in s and "prevent_destroy = true" in s


def test_the_azure_identity_and_disk_set_may_use_only_this_tenants_key():
    s = src()
    roles = re.findall(r'resource "azurerm_role_assignment" "(\w+)" \{(.*?)\n\}', s, re.S)
    assert len(roles) == 2
    for name, body in roles:
        assert "azurerm_key_vault_key.cell.resource_versionless_id" in body, name
        assert '"Key Vault Crypto Service Encryption User"' in body, name


def test_repositories_are_the_tenants_own_and_tags_cannot_change():
    s = src()
    assert 'image_tag_mutability = "IMMUTABLE"' in s and "immutable_tags = true" in s
    assert '"${local.tenant}/postgres"' in s
    assert "kms_key         = aws_kms_key.cell.arn" in s
    assert "scan_on_push = true" in s


def test_the_image_comes_in_by_digest_and_tags_are_refused():
    s = src()
    body = dict(blocks(s, "variable"))["postgres_image_digest"]
    assert not re.search(r"^\s*default\s*=", body, re.M)
    assert "sha256:[0-9a-f]{64}" in body and "Tags are refused" in body
    assert s.count("@${var.postgres_image_digest}") == 3


def test_the_outputs_are_what_the_postgres_module_takes_for_each_cloud():
    s = src()
    pg = POSTGRES.read_text()
    wanted = {
        "aws": ["kms_key_arn", "registry_repository_arn"],
        "azure": ["key_vault_key_id", "identity_id", "disk_encryption_set_id", "administrator"],
        "gcp": ["kms_key_id", "administrator_email", "registry_repository_id"],
    }
    outputs = dict(blocks(s, "output"))
    for cloud, fields in wanted.items():
        for field in fields:
            assert re.search(rf"\b{field}\b", outputs[cloud]), (cloud, field)
            assert re.search(rf"\b{field}\b", pg), (cloud, field)


def test_the_cell_administrator_is_an_entra_group_a_person_makes_and_a_service_account():
    s = src()
    assert 'principal_type = "Group"' in s
    assert 'resource "google_service_account" "administrator"' in s
    # Neither administrator is given a right to use the tenant's key.
    assert "administrator" not in "".join(b for _, b in blocks(s, "resource") if "role_assignment" in b or "iam_member" in b)


def test_both_google_service_agents_may_use_only_this_tenants_key():
    s = src()
    body = re.search(r'resource "google_kms_crypto_key_iam_member" "agents" \{(.*?)\n\}', s, re.S).group(1)
    assert "compute-system.iam.gserviceaccount.com" in body and "gcp-sa-secretmanager.iam.gserviceaccount.com" in body
    assert "crypto_key_id = google_kms_crypto_key.cell.id" in body
    assert 'role          = "roles/cloudkms.cryptoKeyEncrypterDecrypter"' in body

