"""Rules for the cells layer (I-01va) and the two inputs it added to modules/postgres."""
import re
from pathlib import Path

from test_envs import ENVS, blocks, text

ROOT = Path(__file__).resolve().parent.parent
LAYER = ENVS / "europe" / "cells"
PG = ROOT / "infra" / "modules" / "postgres"


def src():
    return text(LAYER)


def test_one_self_hosted_cell_in_each_cloud_reusing_the_environments_private_access():
    s = src()
    mods = dict(blocks(s, "module"))
    for cloud in ("aws", "azure", "gcp"):
        body = mods[f"cell_{cloud}"]
        assert 'source = "../../../modules/postgres"' in body
        assert f'cloud                = "{cloud}"' in body
        assert 'provider_kind        = "self_hosted"' in body
        assert "reuse_private_access = true" in body
        assert "tenant               = local.tenant" in body


def test_it_makes_no_key_repository_or_identity_and_writes_no_tenant_id():
    s = src()
    made = set(re.findall(r'^resource "(\w+)" "(\w+)"', s, re.M))
    assert made == {("terraform_data", "image")}, made
    assert not re.search(r"ten_[0-9a-z]+", s.replace("trimprefix(local.tenant, \"ten_\")", ""))
    assert "local.cell_first.tenant" in s
    # The tenant's own key, repository and administrator, from the cell-first layer and no other.
    for want in ("local.cell_first.aws.kms_key_arn", "local.cell_first.azure.key_vault_key_id", "local.cell_first.gcp.kms_key_id"):
        assert want in s
    assert "control_plane.outputs" not in s and 'data "terraform_remote_state" "control_plane"' not in s


def test_the_image_digest_must_match_the_manifest_before_any_cell_is_made():
    s = src()
    body = re.search(r'resource "terraform_data" "image" \{(.*?)\n\}', s, re.S).group(1)
    assert "endswith(local.cell_first.aws.image" in body and "@${local.postgres.digest}" in body
    assert "local.images.postgres" in s
    for cloud in ("aws", "azure", "gcp"):
        assert f"terraform_data.image.output.{cloud}" in s


def test_the_machine_images_come_from_the_manifest_and_the_ssh_key_is_a_sensitive_variable():
    s = src()
    assert "local.flatcar.version" in s and "family" not in s
    v = dict(blocks(s, "variable"))
    assert re.search(r"sensitive\s*=\s*true", v["administrator_ssh_public_key"])
    for body in v.values():
        assert not re.search(r"^\s*default\s*=", body, re.M)


def test_the_proving_sizes_are_marked_and_override_the_size_table_only_here():
    s = src()
    assert "proving sizes" in re.sub(r"\s+", " ", (LAYER / "cells.tf").read_text())
    assert 'machine_sizes        = { aws = "t3.medium" }' in s
    assert (PG / "main.tf").read_text().count("coalesce(var.machine_sizes.") == 3


def test_the_modules_make_their_own_private_access_unless_told_to_reuse_the_environments():
    for cloud in ("aws", "azure", "gcp"):
        d = PG / f"{cloud}-self-hosted"
        main = (d / "main.tf").read_text()
        assert re.search(rf'module "private_access" \{{\s*source = "../../private-access-{cloud}"\s*count  = var.reuse_private_access \? 0 : 1', main)
        var = dict(blocks((d / "variables.tf").read_text(), "variable"))["reuse_private_access"]
        assert re.search(r"default\s*=\s*false", var)
    assert "module.private_access[*].client_security_group_id" in (PG / "aws-self-hosted" / "main.tf").read_text()
    wrapper = (PG / "main.tf").read_text()
    assert wrapper.count("= var.reuse_private_access") == 3


def test_the_size_table_and_the_new_inputs_defaults_are_unchanged():
    wrapper_vars = dict(blocks((PG / "variables.tf").read_text(), "variable"))
    assert re.search(r"default\s*=\s*false", wrapper_vars["reuse_private_access"])
    assert re.search(r"default\s*=\s*\{\}", wrapper_vars["machine_sizes"])
    table = (PG / "main.tf").read_text()
    assert 'aws_vm = "m6i.large", azure_vm = "Standard_D2ds_v5", gcp_vm = "n2-standard-2"' in table
