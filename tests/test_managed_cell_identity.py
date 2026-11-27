"""K-24ba: a managed AWS cell lets the API machine's own role sign in to its database as one
database role on one cluster, and no other identity. A self-hosted cell is unchanged."""
import re
import shutil
import subprocess
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent
MODULES = ROOT / "infra" / "modules"
API_AWS = MODULES / "api" / "aws"
MANAGED = MODULES / "postgres" / "aws-managed"
API_LAYER = ROOT / "infra" / "envs" / "europe" / "api"
CELLS_LAYER = ROOT / "infra" / "envs" / "europe" / "cells"


def code(path: Path) -> str:
    return "\n".join(l for l in path.read_text().splitlines() if not l.lstrip().startswith("#"))


def test_the_cluster_turns_on_iam_database_authentication():
    assert "iam_database_authentication_enabled = true" in code(MANAGED / "main.tf")


def test_the_machine_may_connect_as_one_role_on_one_cluster_and_nothing_wider():
    text = code(API_AWS / "main.tf")
    doc = re.search(r'data "aws_iam_policy_document" "database" \{(.*?)\n\}\n', text, re.S).group(1)
    assert doc.count("statement {") == 1
    assert 'actions   = ["rds-db:connect"]' in doc
    # The resource names the region, the account, the cluster and the role, with no wildcard.
    assert "dbuser:${var.database_resource_id}/${var.database_role}" in doc
    assert "*" not in doc
    # The grant goes to the machine's own role as an inline policy, and no other principal gets one.
    assert re.search(r'resource "aws_iam_role_policy" "database" \{[^}]*role\s+= aws_iam_role\.api\.id', text, re.S)
    assert "rds-db:" not in text.replace(doc, "")


def test_the_grant_is_made_only_when_the_cell_is_managed():
    text = code(API_AWS / "main.tf")
    for kind, name in (("data", "aws_caller_identity"), ("data", "aws_iam_policy_document"), ("resource", "aws_iam_role_policy")):
        label = "this" if name == "aws_caller_identity" else "database"
        block = re.search(rf'{kind} "{name}" "{label}" \{{\s*count = (.*?)\n', text)
        assert block and block.group(1) == "var.database_resource_id == null ? 0 : 1", (name, label)
    variables = code(API_AWS / "variables.tf")
    assert re.search(r'variable "database_resource_id" \{.*?default\s+= null', variables, re.S)


def test_the_database_role_has_a_default_and_is_never_the_administrator():
    variables = code(API_AWS / "variables.tf")
    assert re.search(r'variable "database_role" \{.*?default\s+= "ledger_app"', variables, re.S)
    assert "ledger_admin" not in code(API_AWS / "main.tf")


def test_the_cluster_id_is_an_identifier_and_reaches_the_api_layer_through_the_cells_outputs():
    assert "aws_rds_cluster.this.cluster_resource_id" in code(MANAGED / "outputs.tf")
    assert "one(module.aws_managed[*].resource_id)" in code(MODULES / "postgres" / "outputs.tf")
    assert "database_resource_id = module.cell_aws.database_resource_id" in code(CELLS_LAYER / "outputs.tf")
    layer = code(API_LAYER / "main.tf")
    assert 'key    = "${local.geo}/cells"' in layer
    aws = re.search(r'module "api_aws" \{(.*?)\n\}', layer, re.S).group(1)
    assert 'local.cells["aws"].kind == "managed"' in aws
    # Azure and Google Cloud are not touched by this task.
    for cloud in ("azure", "gcp"):
        assert "database_resource_id" not in re.search(rf'module "api_{cloud}" \{{(.*?)\n\}}', layer, re.S).group(1)


def test_no_secret_is_a_value_or_an_output_of_the_new_parts():
    for path in (API_AWS / "outputs.tf", MANAGED / "outputs.tf", CELLS_LAYER / "outputs.tf"):
        assert "password" not in code(path).lower().replace("administrator's credential", "")
    assert "database_resource_id" not in code(API_AWS / "outputs.tf")


@pytest.mark.skipif(shutil.which("tofu") is None, reason="tofu is not installed")
def test_the_api_aws_module_validates(tmp_path):
    work = tmp_path / "m"
    shutil.copytree(API_AWS, work, ignore=shutil.ignore_patterns(".terraform*"))
    # The boot module is a sibling in the repository, so keep its relative path working.
    (tmp_path / "api-boot").symlink_to(MODULES / "api-boot")
    src = (work / "main.tf").read_text().replace('"../../api-boot"', '"../api-boot"')
    (work / "main.tf").write_text(src)
    subprocess.run(["tofu", "init", "-backend=false", "-input=false"], cwd=work, check=True, capture_output=True)
    subprocess.run(["tofu", "validate"], cwd=work, check=True, capture_output=True)
