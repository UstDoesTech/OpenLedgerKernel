"""The gated infrastructure workflow keeps its safety properties, including the ones that
matter because this repository is public (anyone can read its logs).

These read .github/workflows/infra-apply.yml as data. They do not run it.
"""

import re
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parent.parent
PATH = ROOT / ".github" / "workflows" / "infra-apply.yml"
WORKFLOWS = ROOT / ".github" / "workflows"


def load():
    return yaml.safe_load(PATH.read_text())


def triggers(wf):
    # YAML reads the key `on` as the boolean True.
    return wf.get("on", wf.get(True))


def steps(job):
    return job["steps"]


def run_text(job):
    return "\n".join(s.get("run", "") for s in steps(job))


def test_only_a_person_dispatching_it_starts_a_run():
    assert set(triggers(load())) == {"workflow_dispatch"}


# The only workflows with cloud access. publish-images only copies images (tests in
# test_publish_images_workflow.py); only infra-apply may apply.
CLOUD_ACCESS = {PATH.name, "publish-images.yml"}


def test_it_is_the_only_workflow_that_applies_and_only_two_have_cloud_access():
    for wf in WORKFLOWS.glob("*.yml"):
        text = wf.read_text()
        if wf.name != PATH.name:
            assert "tofu apply" not in re.sub(r"(?m)^\s*#.*$", "", text), wf.name
        if wf.name in CLOUD_ACCESS:
            continue
        for needle in ("configure-aws-credentials", "azure/login", "google-github-actions/auth"):
            assert needle not in text, f"{wf.name} holds cloud access"


def test_apply_waits_for_the_approved_environment_and_for_the_plan():
    jobs = load()["jobs"]
    assert jobs["apply"]["environment"] == "infra-apply"
    assert jobs["apply"]["needs"] == "plan"
    assert "inputs.action == 'apply'" in jobs["apply"]["if"]
    # The plan job uses its own read-only environment.
    assert jobs["plan"]["environment"] == "infra-plan"


def test_both_jobs_run_only_from_main():
    for job in load()["jobs"].values():
        assert "github.ref == 'refs/heads/main'" in job["if"]
        assert "github.actor == github.repository_owner" in job["if"]


def test_the_plan_is_read_only_and_apply_plans_again_and_compares():
    jobs = load()["jobs"]
    assert "-lock=false" in run_text(jobs["plan"])
    assert "tofu apply" not in run_text(jobs["plan"])
    apply = run_text(jobs["apply"])
    assert "REVIEWED_PLAN_SHA" in apply and "exit 1" in apply
    assert apply.index("exit 1") < apply.index('tofu -chdir="$ROOT" apply')
    assert "-lock=false" not in apply, "the apply plan must take the state lock"
    assert apply.count('apply -no-color -input=false "$RUNNER_TEMP/tfplan"') == 1, "applies only the saved plan"
    assert "-target" not in apply
    assert "-auto-approve" not in apply


def test_nothing_is_uploaded_and_no_cloud_secret_is_stored():
    text = PATH.read_text()
    assert "upload-artifact" not in text
    assert "actions/upload" not in text
    assert "cache" not in " ".join(
        s.get("uses", "") for job in load()["jobs"].values() for s in steps(job)
    )
    code = re.sub(r"(?m)^\s*#.*$", "", text)
    # Federation only: variables name the identity, secrets hold only TF_VAR_ inputs.
    assert "secrets." not in code, "secrets reach the job only through the TF_VAR_ step"
    for bad in ("AWS_SECRET_ACCESS_KEY", "AWS_ACCESS_KEY_ID", "client-secret", "credentials_json", "creds:"):
        assert bad not in code, bad


def test_only_tf_var_secrets_are_passed_on():
    for job in load()["jobs"].values():
        step = next(s for s in steps(job) if s.get("name", "").startswith("Pass the TF_VAR_"))
        assert step["env"] == {"ALL_SECRETS": "${{ toJSON(secrets) }}"}
        assert 'name.startswith("TF_VAR_")' in step["run"]
        # GitHub stores secret names in capitals; OpenTofu variable names are lower case.
        assert '.lower()' in step["run"]
        # Only this step may see every secret.
        others = [s for s in steps(job) if s is not step]
        assert all("toJSON(secrets)" not in str(s) for s in others)


def test_third_party_actions_are_pinned_to_a_commit():
    for job in load()["jobs"].values():
        for s in steps(job):
            if "uses" in s:
                assert re.fullmatch(r"[\w./-]+@[0-9a-f]{40}", s["uses"]), s["uses"]


def test_permissions_are_the_least_that_works():
    wf = load()
    assert wf["permissions"] == {"contents": "read"}
    for job in wf["jobs"].values():
        assert job["permissions"] == {"contents": "read", "id-token": "write"}


def test_inputs_are_checked_before_use_and_never_put_in_a_script_directly():
    code = PATH.read_text()
    assert "${{ inputs." not in re.sub(r"(?m)^\s*(INPUT_\w+|group):.*$", "", code), (
        "inputs are used only through environment variables"
    )
    for job in load()["jobs"].values():
        check = next(s for s in steps(job) if s.get("name") == "Check the inputs")["run"]
        assert "[a-z][a-z0-9-]{0,30}" in check
        assert "deployment.json" in check


def test_runs_for_one_layer_never_overlap_or_cancel():
    c = load()["concurrency"]
    assert c["cancel-in-progress"] is False
    assert "inputs.layer" in c["group"]


def test_the_key_vault_is_opened_for_the_run_only_and_closed_again_even_after_a_failure():
    for name, job in load()["jobs"].items():
        opener = next(s for s in steps(job) if s.get("id") == "keyvault")
        closer = next(s for s in steps(job) if s.get("name", "").startswith("Close the Azure Key Vault"))
        order = [s.get("name") for s in steps(job)]
        # Opened after the Azure sign-in and before anything that reads the vault; closed last.
        assert order.index("Azure (federated)") < order.index(opener["name"]) < order.index("Initialise")
        assert steps(job)[-1] is closer, name
        # Only when the Azure identity and the vault are set; the address is removed whatever happened.
        assert "vars.AZURE_CLIENT_ID" in opener["if"] and "vars.AZURE_KEY_VAULT_NAME" in opener["if"]
        assert "always()" in closer["if"] and "steps.keyvault.outputs.ip" in closer["if"]
        # The address is a single host, from this runner, never a range and never a standing rule.
        assert '"$ip/32"' in opener["run"] and '"$RUNNER_IP/32"' in closer["run"]
        assert "network-rule add" in opener["run"] and "network-rule remove" in closer["run"]
        assert "0.0.0.0" not in opener["run"] + closer["run"]
        # The output is written before the rule is added, so a half-finished step is still closed.
        assert opener["run"].index("GITHUB_OUTPUT") < opener["run"].index("network-rule add")
        # Inputs reach the shell through env, not by expression inside the script.
        assert "${{" not in opener["run"] + closer["run"]


def test_opening_the_vault_adds_no_stored_credential():
    text = PATH.read_text()
    for bad in ("AZURE_CLIENT_SECRET", "az login", "ARM_CLIENT_SECRET", "--default-action"):
        assert bad not in text, bad


# --- Guards for a public repository: nothing private reaches the log or the summary. ---

PLAN_COMMANDS = re.compile(r"\btofu\b.*\b(init|plan|show|apply|output|state)\b")


def run_lines(job):
    for s in steps(job):
        text = s.get("run", "")
        # Join backslash continuations so a command is one line.
        text = re.sub(r"\\\n\s*", " ", text)
        for line in text.splitlines():
            yield s.get("name"), line.strip()


def test_every_tofu_command_goes_through_quiet_or_a_file():
    for job in load()["jobs"].values():
        for name, line in run_lines(job):
            if line.startswith("#") or not PLAN_COMMANDS.search(line):
                continue
            ok = line.startswith("scripts/quiet.sh ") or "> \"$" in line
            assert ok, f"{name}: {line}"


def test_nothing_prints_a_plan_or_a_log_file():
    for job in load()["jobs"].values():
        for name, line in run_lines(job):
            if line.startswith("#"):
                continue
            if re.search(r"\b(cat|head|tail|tee|less|more|sed|awk|jq|echo|printf)\b", line) and re.search(
                r"plan\.txt|plan\.json|tfplan|\.log\b|LOG_DIR", line
            ):
                # The only reads allowed: hashing the plan text, and the numbers-only apply line.
                allowed = "sha256sum" in line or "grep -m1 -E" in line or line.endswith('>> "$GITHUB_ENV"')
                assert allowed, f"{name}: {line}"


def test_the_apply_job_prints_only_the_numbers_line():
    apply = run_text(load()["jobs"]["apply"])
    assert "grep -m1 -E '^Apply complete! Resources: [0-9]+ added, [0-9]+ changed, [0-9]+ destroyed\\.$' \"$LOG_DIR/apply.log\"" in apply


def test_the_summary_holds_only_counts_a_fingerprint_and_the_run_id():
    run = next(s for s in steps(load()["jobs"]["plan"]) if s.get("name") == "Plan")["run"]
    written = [l for l in run.splitlines() if "GITHUB_STEP_SUMMARY" in l]
    assert len(written) == 1 and written[0].strip() == "} >> \"$GITHUB_STEP_SUMMARY\""
    block = run[run.index("{\n"):run.index("} >> ")]
    assert "plan_counts.py" in block and "$sha" in block and "$GITHUB_RUN_ID" in block
    assert "plan.txt" not in block and "cat " not in block


def test_the_plan_json_stays_on_the_runner_and_is_never_kept():
    keep = (ROOT / "scripts" / "keep_logs.sh").read_text()
    assert "plan.json" not in keep
    for job in load()["jobs"].values():
        for name, line in run_lines(job):
            if "plan.json" in line and not line.startswith("#"):
                assert "RUNNER_TEMP/plan.json" in line and "LOG_DIR" not in line, f"{name}: {line}"


def test_logs_are_kept_in_the_plan_store_before_the_vault_closes():
    for job in load()["jobs"].values():
        order = [s.get("name") for s in steps(job)]
        keep = next(s for s in steps(job) if s.get("name", "").startswith("Keep the logs"))
        assert keep["if"] == "always()"
        assert order.index(keep["name"]) == len(order) - 2
        assert keep["run"].startswith("scripts/keep_logs.sh ")


def test_the_plan_store_is_required_and_must_be_s3():
    for job in load()["jobs"].values():
        check = next(s for s in steps(job) if s.get("name") == "Check the inputs")["run"]
        assert "^s3://[a-z0-9.-]+(/[A-Za-z0-9._/-]*)?$" in check
        assert "AWS_ROLE_ARN" in check
        assert job["env"]["PLAN_STORE"] == "${{ vars.PLAN_STORE }}"


def test_azure_commands_that_print_settings_have_their_output_dropped():
    for job in load()["jobs"].values():
        for name, line in run_lines(job):
            if line.startswith("az keyvault network-rule"):
                assert "-o none" in line, f"{name}: {line}"


def test_tofu_output_is_never_in_colour_or_interactive():
    for job in load()["jobs"].values():
        for name, line in run_lines(job):
            if line.startswith("scripts/quiet.sh") and " tofu " in line:
                assert "-no-color" in line and "-input=false" in line, f"{name}: {line}"


def test_no_pull_request_trigger_anywhere_and_no_pull_request_target():
    for wf in WORKFLOWS.glob("*.yml"):
        text = re.sub(r"(?m)^\s*#.*$", "", wf.read_text())
        assert "pull_request_target" not in text, wf.name
        if wf.name in CLOUD_ACCESS:
            assert "pull_request" not in text, wf.name
            assert "\npush:" not in text and "  push:" not in text, wf.name


def test_the_code_owners_cover_workflows_scripts_and_infrastructure():
    owners = (ROOT / ".github" / "CODEOWNERS").read_text()
    for path in ("/.github/", "/infra/", "/scripts/quiet.sh", "/scripts/keep_logs.sh", "/scripts/plan_counts.py"):
        assert re.search(rf"(?m)^{re.escape(path)}\s+@UstDoesTech$", owners), path


def test_the_runbook_names_the_exact_trust_subjects_and_allows_no_wildcard():
    doc = (ROOT / "docs" / "credentialed-workflows.md").read_text()
    for env in ("infra-apply", "infra-plan"):
        assert f"repo:UstDoesTech/OpenLedgerKernel:environment:{env}" in doc or f":environment:{env}" in doc
    assert "repo:UstDoesTech/OpenLedgerKernel:environment:infra-apply" in doc
    assert "assertion.repository == 'UstDoesTech/OpenLedgerKernel'" in doc
    assert not re.search(r"repo:UstDoesTech/OpenLedgerKernel:(\*|ref:|pull_request)", doc)
    assert "environment:*" not in doc and "repo:*" not in doc
