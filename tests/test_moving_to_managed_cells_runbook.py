"""The runbook for moving to managed cells and back (I-01vd) names real workflows, inputs, settings, files
and counts, says what is not ready yet, and holds no secret or name."""
import re
from pathlib import Path

import pytest
import yaml

ROOT = Path(__file__).resolve().parent.parent
RUNBOOKS = ROOT / "docs" / "runbooks"
RUNBOOK = RUNBOOKS / "moving-to-managed-cells.md"
TEXT = RUNBOOK.read_text()

WORKFLOWS = {
    "infra-apply": ["env", "layer", "action", "replace"],
    "rotate-credentials": ["env", "kind", "proving"],
}


def inputs_of(name):
    workflow = yaml.safe_load((ROOT / ".github" / "workflows" / f"{name}.yml").read_text())
    trigger = workflow.get(True) or workflow["on"]
    return (trigger["workflow_dispatch"] or {}).get("inputs") or {}


@pytest.mark.parametrize("name", sorted(WORKFLOWS))
def test_each_workflow_it_names_exists_and_has_the_inputs_it_sets(name):
    assert f"**`{name}`**" in TEXT, f"the runbook should name {name}"
    have = inputs_of(name)
    for wanted in WORKFLOWS[name]:
        if wanted == "replace":
            continue  # named in the rolling runbook it links to
        assert wanted in have, f"{name} has no input {wanted}"
        assert f"`{wanted}`" in TEXT


def test_it_sets_only_values_the_workflows_allow():
    assert {"plan", "apply"} <= set(inputs_of("infra-apply")["action"]["options"])
    assert {"credentials", "all"} <= set(inputs_of("rotate-credentials")["kind"]["options"])
    assert "`kind` = `credentials`" in TEXT and "`kind` = `all`" in TEXT
    # It never turns the proving option on for a managed cell: the run must not read the administrator.
    assert "Leave `proving` off" in TEXT


def test_the_layers_it_names_exist():
    for layer in ["cells", "api"]:
        assert (ROOT / "infra" / "envs" / "europe" / layer / "main.tf").is_file() or (
            ROOT / "infra" / "envs" / "europe" / layer / "cells.tf"
        ).is_file(), layer
        assert f"`layer` = `{layer}`" in TEXT
    for layer in ["network", "interconnect", "private-access"]:
        assert (ROOT / "infra" / "envs" / "europe" / layer).is_dir(), layer


def test_the_settings_it_names_exist_with_the_defaults_it_relies_on():
    variables = (ROOT / "infra" / "envs" / "europe" / "cells" / "variables.tf").read_text()
    kind = re.search(r'variable "cell_kind" \{(.*?)\n\}', variables, re.S).group(1)
    assert 'default     = "self_hosted"' in kind and "managed" in kind
    protection = re.search(r'variable "deletion_protection" \{(.*?)\n\}', variables, re.S).group(1)
    assert "default     = true" in protection
    for secret in ["TF_VAR_cell_kind", "TF_VAR_deletion_protection", "TF_VAR_create_database_role", "TF_VAR_migrate_database"]:
        assert f"`{secret}`" in TEXT, secret
    api = (ROOT / "infra" / "envs" / "europe" / "api" / "variables.tf").read_text()
    for name in ["create_database_role", "migrate_database"]:
        assert f'variable "{name}"' in api, name


def test_deletion_protection_is_only_where_the_runbook_says():
    root = ROOT / "infra" / "modules" / "postgres"
    assert "deletion_protection" in (root / "aws-managed" / "main.tf").read_text()
    assert "deletion_protection" in (root / "gcp-managed" / "main.tf").read_text()
    assert "deletion_protection" not in (root / "azure-managed" / "main.tf").read_text()
    assert 'mode                      = "ZoneRedundant"' in (root / "azure-managed" / "main.tf").read_text()
    assert "Azure cannot stop a server that has it" in TEXT


def test_the_status_table_matches_what_the_code_does():
    # Only Amazon has a CA bundle, and the refusal is worded as the runbook quotes it.
    script = (ROOT / "scripts" / "fetch_database_ca.py").read_text()
    assert re.search(r"BUNDLES = \{\s*\"aws\":", script) and script.count('": "https://') == 1
    assert "a managed database in {cloud} has no public CA bundle this run can fetch yet" in script
    assert "a managed database in azure has no public CA bundle this run can fetch yet" in TEXT
    # Only AWS has the sign-in grant, and the managed Azure and Google Cloud modules have none.
    aws = (ROOT / "infra" / "modules" / "api" / "aws" / "main.tf").read_text()
    assert "rds-db:connect" in aws
    for cloud in ["azure", "gcp"]:
        assert "database_resource_id" not in (ROOT / "infra" / "modules" / "api" / cloud / "variables.tf").read_text()
    # The cell kind is one setting: each cell module reads it.
    cells = (ROOT / "infra" / "envs" / "europe" / "cells" / "cells.tf").read_text()
    assert cells.count("provider_kind        = var.cell_kind") == 3
    assert "one setting for all three cells" in TEXT


def test_it_names_the_plan_tasks_that_will_change_it_and_they_exist():
    plan = yaml.safe_load((ROOT / "plan" / "tasks.yaml").read_text())
    tasks = {t["id"] for t in (plan["tasks"] if isinstance(plan, dict) else plan)}
    for task in ["K-24g", "K-24bb", "K-24bc", "K-24h", "K-24i", "I-01ve"]:
        assert task in TEXT, task
        assert task in tasks, task


def test_the_counts_it_reads_are_ones_the_rotation_prints():
    report = (ROOT / "crates" / "pki" / "src" / "rotate.rs").read_text() + (ROOT / "crates" / "pki" / "src" / "main.rs").read_text()
    for label in ["database node certificates issued", "database connections made", "cells still missing a file"]:
        assert label in report, label
        assert f"`{label}`" in TEXT
    assert "managed role: statements run: {}, failed: 0" in (ROOT / "crates" / "api" / "src" / "application_role.rs").read_text()
    assert "ledger-api: managed role: statements run: 10, failed: 0" in TEXT
    assert "steps: N, failed: 0" in (RUNBOOKS / "first-entry.md").read_text()


def test_it_says_what_the_user_gives_up():
    for part in [
        "No data is carried across",
        "The control plane is not touched",
        "never the entries",
        "Azure cannot stop a server that has it",
        "seven days",
        "db-sign-in",
        "by hand",
    ]:
        assert part in TEXT, part
    # And the claim about the control plane is true: the cells layer has nothing to do with etcd.
    for source in (ROOT / "infra" / "envs" / "europe" / "cells").glob("*.tf"):
        body = source.read_text().lower()
        assert "etcd" not in body and "control_plane" not in body, source.name


def test_going_back_leaves_the_sign_in_files_which_is_why_it_says_to_delete_them():
    rotate = (ROOT / "crates" / "pki" / "src" / "rotate.rs").read_text()
    assert "never removes one" in rotate
    assert 'let Some(sign_in) = db.sign_in else { continue };' in rotate


def test_every_file_and_link_it_names_exists():
    paths = set(re.findall(r"`((?:infra|scripts|docs|crates|tests|\.github)/[A-Za-z0-9_./-]+)`", TEXT))
    assert "scripts/first_entry_check.sh" in paths
    assert "scripts/proving_power.sh" in TEXT and (ROOT / "scripts" / "proving_power.sh").is_file()
    for path in paths:
        assert (ROOT / path).exists(), path
    for target in re.findall(r"\]\(([^)#]+)(?:#[^)]*)?\)", TEXT):
        if target.startswith("http"):
            continue
        assert (RUNBOOKS / target).resolve().exists(), target


def test_the_anchors_it_links_to_exist():
    for target, anchor in re.findall(r"\]\(([^)#]+)#([^)]+)\)", TEXT):
        headings = {
            re.sub(r"[^a-z0-9 -]", "", h.lower()).strip().replace(" ", "-")
            for h in re.findall(r"^#+ (.+)$", (RUNBOOKS / target).read_text(), re.M)
        }
        assert anchor in headings, (target, anchor)
    own = {
        re.sub(r"[^a-z0-9 -]", "", h.lower()).strip().replace(" ", "-")
        for h in re.findall(r"^#+ (.+)$", TEXT, re.M)
    }
    assert "what-is-ready-and-what-is-not" in own


def test_it_is_in_the_index():
    assert "(moving-to-managed-cells.md)" in (RUNBOOKS / "README.md").read_text()


def test_it_prints_no_secret_and_no_name():
    assert not re.search(r"sha256:[0-9a-f]{64}", TEXT)
    for word in ["bankofmumanddad", "kv-ledger", "ustdoes.tech", "arn:aws", "AKIA"]:
        assert word not in TEXT, word
    assert "the business" not in TEXT.lower()
