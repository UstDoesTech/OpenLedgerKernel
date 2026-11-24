"""The runbook for putting the AWS API and page on the internet (K-23c) names real workflows, inputs,
layers, variables, files and machine addresses, and its numbers match the modules, so that no step in
it is one that cannot be done."""
import importlib.util
import re
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parent.parent
RUNBOOKS = ROOT / "docs" / "runbooks"
TEXT = (RUNBOOKS / "exposing-the-api-in-aws.md").read_text()
LAYER = ROOT / "infra" / "envs" / "europe" / "api"
FRONT = (ROOT / "infra" / "modules" / "api-front-aws" / "main.tf").read_text()
EDGE = (ROOT / "infra" / "modules" / "edge-aws" / "main.tf").read_text()

spec = importlib.util.spec_from_file_location("replace_guard", ROOT / "scripts" / "replace_guard.py")
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)


def inputs_of(name):
    workflow = yaml.safe_load((ROOT / ".github" / "workflows" / f"{name}.yml").read_text())
    trigger = workflow.get(True) or workflow["on"]
    return (trigger["workflow_dispatch"] or {}).get("inputs") or {}


def test_the_workflow_and_its_inputs_exist():
    assert "**`infra-apply`**" in TEXT
    have = inputs_of("infra-apply")
    for name in ("env", "layer", "action"):
        assert name in have and f"`{name}`" in TEXT
    assert {"plan", "apply"} <= set(have["action"]["options"])
    assert "europe" in have["env"].get("options", ["europe"])
    assert "replace" in have


def test_the_layers_it_names_exist_and_are_applied_in_the_order_it_gives():
    for layer in ("network", "api", "interconnect", "private-access"):
        assert (ROOT / "infra" / "envs" / "europe" / layer / "main.tf").is_file(), layer
        assert f"`{layer}`" in TEXT
    assert TEXT.index("`layer` = `network`") < TEXT.index("`layer` = `api`")
    for step in ("## 1.", "## 2.", "## 3.", "## 4.", "## 5.", "## 6.", "## 7.", "## 8."):
        assert step in TEXT
    order = [TEXT.index(f"## {n}.") for n in range(1, 9)]
    assert order == sorted(order)


def test_every_setting_it_names_is_a_variable_with_the_default_it_says():
    variables = (LAYER / "variables.tf").read_text()
    for name in ("edge_enabled", "front_domain_name", "certificate_validated", "edge_client_cidrs", "open_to_internet"):
        assert f"TF_VAR_{name}" in TEXT, name
        assert f'variable "{name}"' in variables, name
    block = lambda n: variables[variables.index(f'variable "{n}"'):].split("\n}\n")[0]
    assert re.search(r"default\s+= false", block("edge_enabled"))
    assert re.search(r"default\s+= false", block("certificate_validated"))
    assert re.search(r"default\s+= false", block("open_to_internet"))
    assert 'default     = ""' in block("front_domain_name")
    assert re.search(r"default\s+= \[\]", block("edge_client_cidrs"))


def test_only_secrets_with_the_prefix_reach_opentofu_and_the_workflow_lowers_the_name():
    workflow = (ROOT / ".github" / "workflows" / "infra-apply.yml").read_text()
    assert 'name.startswith("TF_VAR_")' in workflow and '"TF_VAR_" + name[len("TF_VAR_"):].lower()' in workflow
    assert "TF_VAR_" in TEXT and "lowers the part after the prefix" in TEXT


def test_the_resource_counts_it_gives_are_the_ones_the_modules_make():
    # Step 2: the edge module and the certificate.
    edge = len(re.findall(r'^resource "', EDGE, re.M))  # gateway, table, route, association
    association = EDGE[EDGE.index('resource "aws_route_table_association"'):]
    assert "count = length(var.subnet_ids)" in association
    subnets = 3
    certificate = 1
    assert edge - 1 + subnets + certificate == 7
    assert "`7 to add, 0 to change, 0 to destroy`" in TEXT
    # Step 5: everything else, with no rule for a range or the open switch.
    live = len(re.findall(r"count\s+= local\.live", FRONT))
    assert live == 9
    assert "nine resources" in TEXT


def test_it_says_what_the_modules_do():
    assert "ELBSecurityPolicy-TLS13-1-2-Res-2021-06" in FRONT and "ELBSecurityPolicy-TLS13-1-2-Res-2021-06" in TEXT
    assert "30 minutes" in TEXT and 'create = "30m"' in FRONT
    assert re.search(r"default\s+= 300", (ROOT / "infra" / "modules" / "api-front-aws" / "variables.tf").read_text())
    assert "300 requests in 5 minutes" in TEXT
    assert "LEDGER_TRUSTED_PROXIES" in TEXT
    # The refusal it describes is the one in the module.
    variables = (ROOT / "infra" / "modules" / "api-front-aws" / "variables.tf").read_text()
    assert "!strcontains(c, \":\")" in variables and ">= 8" in variables
    assert "`0.0.0.0/0`, `::/0` and any range wider than /8" in TEXT
    assert "DNS only" in TEXT and "grey" in TEXT


def test_the_machine_address_is_one_the_workflow_accepts_and_is_replaced_before_the_load_balancer():
    addresses = re.findall(r"`(module\.[^`]+)`", TEXT)
    assert addresses == ["module.api_aws.aws_instance.api"]
    assert guard.valid_address(addresses[0])
    assert TEXT.index("## 4. Replace the machine") < TEXT.index("## 5. Make the load balancer")
    assert "K-23e" in TEXT


def test_the_task_it_says_is_not_done_is_in_the_plan_and_the_ones_it_says_are_done_are():
    plan = yaml.safe_load((ROOT / "plan" / "tasks.yaml").read_text())
    status = {t["id"]: t["status"] for t in plan["tasks"]}
    assert status["K-23e"] in {"todo", "in_progress", "done"}
    for done in ("K-23a", "K-23b", "K-23d"):
        assert status[done] == "done", done


def test_every_file_and_link_it_names_exists():
    paths = set(re.findall(r"`((?:infra|scripts|docs|crates|tests|\.github)/[A-Za-z0-9_./-]+)`", TEXT))
    for path in paths:
        assert (ROOT / path).exists(), path
    for target in re.findall(r"\]\(([^)#]+)(?:#[^)]*)?\)", TEXT):
        if target.startswith("http"):
            continue
        assert (RUNBOOKS / target).resolve().exists(), target
    headings = {
        re.sub(r"[^a-z0-9 -]", "", h.lower()).strip().replace(" ", "-")
        for h in re.findall(r"^#+ (.+)$", (RUNBOOKS / "first-entry.md").read_text(), re.M)
    }
    for anchor in re.findall(r"\]\(first-entry\.md#([^)]+)\)", TEXT):
        assert anchor in headings, anchor


def test_it_covers_the_whole_life_of_the_page():
    for part in ("What it exposes, and what it costs", "Taking it off", "If something goes wrong", "Check it",
                 "Make the validation record", "Choose who can reach it", "K-23d"):
        assert part in TEXT, part


def test_it_is_in_the_index_and_writes_no_name_secret_or_account():
    assert "(exposing-the-api-in-aws.md)" in (RUNBOOKS / "README.md").read_text()
    assert "<page name>" in TEXT
    assert not re.search(r"\b[a-z0-9-]+\.(tech|com|org|net|io|uk)\b", TEXT.replace("example.org", "")), "no real name"
    assert not re.search(r"\b\d{12}\b", TEXT), "no account number"
    assert not re.search(r"sha256:[0-9a-f]{64}", TEXT)
    for word in ("bankofmumanddad", "kv-ledger"):
        assert word not in TEXT
