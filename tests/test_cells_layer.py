"""Rules for the cells layer (I-01va) and the two inputs it added to modules/postgres."""
import re
from pathlib import Path

from test_envs import ENVS, blocks, text

ROOT = Path(__file__).resolve().parent.parent
LAYER = ENVS / "europe" / "cells"
PG = ROOT / "infra" / "modules" / "postgres"


def src():
    return text(LAYER)


def test_one_cell_in_each_cloud_reusing_the_environments_private_access():
    s = src()
    mods = dict(blocks(s, "module"))
    for cloud in ("aws", "azure", "gcp"):
        body = mods[f"cell_{cloud}"]
        assert 'source = "../../../modules/postgres"' in body
        assert f'cloud                = "{cloud}"' in body
        assert f'provider_kind        = local.kinds["{cloud}"]' in body
        assert "deletion_protection  = var.deletion_protection" in body
        assert "reuse_private_access = true" in body
        assert "tenant               = local.tenant" in body


def test_it_makes_no_key_repository_or_identity_and_writes_no_tenant_id():
    s = src()
    made = set(re.findall(r'^resource "(\w+)" "(\w+)"', s, re.M))
    # The one grant is for Cloud SQL's service agent on the tenant's own key, made only for managed cells.
    assert made == {("terraform_data", "image"), ("google_kms_crypto_key_iam_member", "cloud_sql")}, made
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
    # Only the kind and its protection have defaults, so that an applied layer is unchanged.
    for name, body in v.items():
        has_default = bool(re.search(r"^\s*default\s*=", body, re.M))
        assert has_default == (name in {"cell_kind", "cell_kinds", "deletion_protection"}), name


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


def test_the_kind_is_a_checked_variable_that_defaults_to_self_hosted():
    v = dict(blocks(src(), "variable"))
    kind = v["cell_kind"]
    assert 'default     = "self_hosted"' in kind
    assert 'contains(["self_hosted", "managed"], var.cell_kind)' in kind
    assert "default     = true" in v["deletion_protection"]


def test_the_managed_form_takes_the_tenants_own_key_and_the_azure_zone_only_when_managed():
    s = src()
    # The managed modules take the same tenant key inputs the self-hosted form does.
    for want in ("local.cell_first.aws.kms_key_arn", "local.cell_first.azure.key_vault_key_id", "local.cell_first.gcp.kms_key_id"):
        assert want in s
    wrapper = (PG / "main.tf").read_text()
    for cloud, key in (("aws", "kms_key_arn"), ("azure", "key_vault_key_id"), ("gcp", "kms_key_id")):
        body = re.search(rf'module "{cloud}_managed" \{{(.*?)\n\}}', wrapper, re.S).group(1)
        assert f"var.{cloud}.{key}" in body
    mods = dict(blocks(s, "module"))
    assert 'source = "../../../modules/postgres/azure-dns"' in mods["azure_dns"]
    assert 'count  = local.kinds["azure"] == "managed" ? 1 : 0' in mods["azure_dns"]
    assert "try(module.azure_dns[0].private_dns_zone_id, null)" in s
    grant = re.search(r'resource "google_kms_crypto_key_iam_member" "cloud_sql" \{(.*?)\n\}', s, re.S).group(1)
    assert 'count = local.kinds["gcp"] == "managed" ? 1 : 0' in grant
    assert "local.cell_first.gcp.kms_key_id" in grant and "gcp-sa-cloud-sql" in grant
    assert "cryptoKeyEncrypterDecrypter" in grant


def test_switching_kind_replaces_the_cell_because_each_form_exists_only_for_its_own_kind():
    wrapper = (PG / "main.tf").read_text()
    for name, kind in (("aws_managed", "managed"), ("azure_managed", "managed"), ("gcp_managed", "managed"),
                       ("aws_self_hosted", "self_hosted"), ("azure_self_hosted", "self_hosted"), ("gcp_self_hosted", "self_hosted")):
        body = re.search(rf'module "{name}" \{{(.*?)\n\}}', wrapper, re.S).group(1)
        assert f'local.kind == "{kind}" ? 1 : 0' in body, name
    # The cell's address and name stay the same, so a plan shows the old form destroyed and the new one made.
    assert 'name                 = "${local.slug}-aws-001"' in src()


def test_each_clouds_kind_can_be_set_on_its_own_and_defaults_to_the_one_setting():
    s = src()
    v = dict(blocks(s, "variable"))
    kinds = v["cell_kinds"]
    assert "type        = map(string)" in kinds and "default     = {}" in kinds
    assert 'contains(["aws", "azure", "gcp"], cloud)' in kinds
    assert 'contains(["self_hosted", "managed"], kind)' in kinds
    # A cloud that is not named takes cell_kind, so an environment that sets nothing new is planned as before.
    assert 'for cloud in ["aws", "azure", "gcp"] : cloud => lookup(var.cell_kinds, cloud, var.cell_kind)' in s
    # Nothing reads cell_kind except that one line and the variable's own check, so no cloud ignores cell_kinds.
    assert len(re.findall(r"var\.cell_kind\b", s)) == 2 and len(re.findall(r"var\.cell_kinds\b", s)) == 3
    # Each cell module and each managed-only helper reads its own cloud's entry and no other.
    mods = dict(blocks(s, "module"))
    for cloud in ("aws", "azure", "gcp"):
        assert f'provider_kind        = local.kinds["{cloud}"]' in mods[f"cell_{cloud}"]
    assert 'local.kinds["azure"]' in mods["azure_dns"] and 'local.kinds["gcp"]' in s


def test_with_only_aws_managed_the_other_cells_keep_the_self_hosted_form():
    """The same lookup, run in Python, over the layer's three clouds."""
    def kinds(cell_kind, cell_kinds):
        return {c: cell_kinds.get(c, cell_kind) for c in ("aws", "azure", "gcp")}

    assert kinds("self_hosted", {}) == {"aws": "self_hosted", "azure": "self_hosted", "gcp": "self_hosted"}
    assert kinds("managed", {}) == {"aws": "managed", "azure": "managed", "gcp": "managed"}
    assert kinds("self_hosted", {"aws": "managed"}) == {"aws": "managed", "azure": "self_hosted", "gcp": "self_hosted"}
    assert kinds("managed", {"gcp": "self_hosted"})["gcp"] == "self_hosted"
    # The wrapper turns each kind into exactly one form, so only the AWS cell changes.
    wrapper = (PG / "main.tf").read_text()
    assert 'local.kind == "managed" ? 1 : 0' in wrapper and 'local.kind == "self_hosted" ? 1 : 0' in wrapper
