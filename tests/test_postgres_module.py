"""I-01d: the Postgres module. OpenTofu validates it in CI; these tests hold the guarantees
that validation cannot: no credential leaves it, nothing public, vanilla PostgreSQL only."""
import re
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent
MODULE = ROOT / "infra" / "modules" / "postgres"
SUBMODULES = sorted(p.name for p in MODULE.iterdir() if p.is_dir())
FORMS = [m for m in SUBMODULES if m != "azure-dns"]


def code(path: Path) -> str:
    return "\n".join(l for l in path.read_text().splitlines() if not l.lstrip().startswith("#"))


def body(directory: Path) -> str:
    return "\n".join(code(p) for p in sorted(directory.glob("*.tf")))


def test_all_seven_forms_exist():
    assert set(FORMS) == {
        "aws-managed", "azure-managed", "gcp-managed", "lakebase",
        "aws-self-hosted", "azure-self-hosted", "gcp-self-hosted",
    }


@pytest.mark.parametrize("form", ["."] + SUBMODULES)
def test_module_structure(form):
    for name in ("versions.tf", "variables.tf", "outputs.tf", "main.tf"):
        assert (MODULE / form / name).exists(), f"{form}/{name}"


@pytest.mark.parametrize("form", FORMS)
def test_outputs_are_host_port_and_secret_reference_only(form):
    text = code(MODULE / form / "outputs.tf")
    # Only a form the platform runs has nodes whose certificates someone must issue (K-20a).
    expected = {"host", "port", "secret_ref"} | ({"nodes"} if form.endswith("-self-hosted") else set())
    assert set(re.findall(r'^output "(\w+)"', text, re.M)) == expected


def test_facade_outputs_are_host_port_secret_ref_and_kind():
    text = code(MODULE / "outputs.tf")
    assert set(re.findall(r'^output "(\w+)"', text, re.M)) == {"kind", "host", "port", "secret_ref", "nodes"}


def test_no_password_or_connection_string_anywhere():
    for tf in MODULE.rglob("*.tf"):
        text = code(tf)
        assert not re.search(r"\bpassword\s*=\s*(?!false)", text, re.I), f"{tf} sets a password"
        assert not re.search(r"postgres(ql)?://", text), f"{tf} builds a connection string"
        assert "random_password" not in text


def test_managed_forms_have_no_password_sign_in():
    assert "manage_master_user_password   = true" in body(MODULE / "aws-managed")
    assert "password_auth_enabled         = false" in body(MODULE / "azure-managed")
    assert "CLOUD_IAM_SERVICE_ACCOUNT" in body(MODULE / "gcp-managed")


def test_vanilla_postgres_16_with_no_extensions_or_logical_slots():
    for tf in MODULE.rglob("*.tf"):
        text = code(tf)
        assert not re.search(r"create extension|shared_preload_libraries|azure\.extensions|pglogical|wal_level\s*=\s*logical|logical_replication|replication_slot", text, re.I), tf
    assert 'family      = "postgres16"' in body(MODULE / "aws-managed")
    assert 'version                       = "16"' in body(MODULE / "azure-managed")
    assert '"POSTGRES_16"' in body(MODULE / "gcp-managed")
    assert "pg_version   = 16" in body(MODULE / "lakebase")


@pytest.mark.parametrize("form", ["aws-managed", "azure-managed", "gcp-managed", "aws-self-hosted", "azure-self-hosted", "gcp-self-hosted"])
def test_nothing_is_public(form):
    text = body(MODULE / form)
    assert not re.search(r"publicly_accessible\s*=\s*true|ipv4_enabled\s*=\s*true|public_network_access_enabled\s*=\s*true|access_config|0\.0\.0\.0/0", text)
    assert not re.search(r"azurerm_public_ip|aws_eip|associate_public_ip_address\s*=\s*true", text)


def test_private_only_is_explicit_where_the_default_is_open():
    assert "publicly_accessible             = false" in body(MODULE / "aws-managed")
    assert "public_network_access_enabled = false" in body(MODULE / "azure-managed")
    assert "ipv4_enabled = false" in body(MODULE / "gcp-managed")
    assert "associate_public_ip_address = false" in body(MODULE / "aws-self-hosted")


def test_storage_is_encrypted_with_the_tenant_key_in_every_form_that_holds_it():
    assert "storage_encrypted                   = true" in body(MODULE / "aws-managed")
    assert "kms_key_id                          = var.kms_key_arn" in body(MODULE / "aws-managed")
    assert "customer_managed_key" in body(MODULE / "azure-managed")
    assert "encryption_key_name = var.kms_key_id" in body(MODULE / "gcp-managed")
    aws = body(MODULE / "aws-self-hosted")
    assert "encrypted  = true" in aws  # the root disk
    volume = aws.split('resource "aws_ebs_volume" "data"')[1].split("\nresource ")[0]
    assert re.search(r"encrypted\s+= true", volume) and "kms_key_id        = var.kms_key_arn" in volume
    assert body(MODULE / "aws-self-hosted").count("var.kms_key_arn") >= 4
    assert "disk_encryption_set_id" in body(MODULE / "azure-self-hosted")
    assert "kms_key_self_link = var.kms_key_id" in body(MODULE / "gcp-self-hosted")


def test_standbys_are_in_other_zones_with_synchronous_commit():
    assert "db_cluster_instance_class" in body(MODULE / "aws-managed")  # Multi-AZ cluster
    assert 'mode                      = "ZoneRedundant"' in body(MODULE / "azure-managed")
    assert 'availability_type           = "REGIONAL"' in body(MODULE / "gcp-managed")
    lakebase = body(MODULE / "lakebase")
    assert re.search(r"\bmin\s*=\s*3\b", lakebase) and re.search(r"\bmax\s*=\s*3\b", lakebase)
    assert "enable_readable_secondaries" in lakebase
    for form in ("aws-self-hosted", "azure-self-hosted", "gcp-self-hosted"):
        text = body(MODULE / form)
        assert 'ANY 1 (' in text, form
        assert "count = 3" in text, form


def test_self_hosted_nodes_have_no_internet_dependency_and_use_imdsv2():
    assert 'http_tokens   = "required"' in body(MODULE / "aws-self-hosted")
    for form in ("aws-self-hosted", "azure-self-hosted", "gcp-self-hosted"):
        assert not re.search(r"apt-get|yum |curl |wget ", body(MODULE / form)), form


def test_provider_kind_default_is_lakebase_only_where_databricks_is_given():
    text = code(MODULE / "main.tf")
    assert 'coalesce(var.provider_kind, var.databricks != null ? "lakebase" : "managed")' in text


def test_lakebase_must_be_attested_private_and_keyed():
    text = code(MODULE / "main.tf")
    assert "private_link" in text and "customer_managed_key" in text
    assert re.search(r'local\.kind != "lakebase" \|\|', text)


def test_every_resource_is_tagged_with_the_tenant():
    text = code(MODULE / "main.tf")
    assert "tenant = var.tenant" in text
    assert "managed_by = \"opentofu\"" in text


def test_tenant_comes_from_a_validated_prefix():
    assert "^ten_" in code(MODULE / "variables.tf")


@pytest.mark.parametrize("form", FORMS)
def test_each_form_pins_only_its_own_provider(form):
    text = code(MODULE / form / "versions.tf")
    assert len(re.findall(r"source\s*=", text)) == 1


def test_every_self_hosted_form_keeps_its_data_disk_as_its_own_resource():
    # A machine is replaced to roll a new node image (docs/runbooks/rolling-the-node-image.md).
    # An inline disk is deleted with its machine on AWS, so each cloud holds the disk apart.
    disks = {
        "aws-self-hosted": ("aws_ebs_volume", "aws_volume_attachment"),
        "azure-self-hosted": ("azurerm_managed_disk", "azurerm_virtual_machine_data_disk_attachment"),
        "gcp-self-hosted": ("google_compute_disk", None),
    }
    for form, (disk, attachment) in disks.items():
        s = body(MODULE / form)
        assert f'resource "{disk}" "data"' in s, form
        if attachment:
            assert f'resource "{attachment}" "data"' in s, form
    assert "ebs_block_device" not in body(MODULE / "aws-self-hosted")
