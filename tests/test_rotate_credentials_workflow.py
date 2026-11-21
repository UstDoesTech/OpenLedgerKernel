"""K-17: the rotation workflow holds cloud access on a public repository, so these tests hold it to
the same rules as infra-apply and to one more: it prints counts only. They read the workflow as
data and do not run it."""
import re
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parent.parent
PATH = ROOT / ".github" / "workflows" / "rotate-credentials.yml"
IDENTIFYING = {
    "AWS_ROLE_ARN", "AZURE_CLIENT_ID", "AZURE_TENANT_ID", "AZURE_SUBSCRIPTION_ID",
    "GCP_WORKLOAD_IDENTITY_PROVIDER", "GCP_SERVICE_ACCOUNT", "TOFU_BACKEND_CONFIG", "PLAN_STORE",
    "AZURE_KEY_VAULT_NAME", "AZURE_KEY_VAULT_RESOURCE_GROUP",
}
PLAIN_VARIABLES = {"AWS_REGION", "TOFU_STATE_KEY_NAME"}


def load():
    return yaml.safe_load(PATH.read_text())


def job():
    return load()["jobs"]["rotate"]


def steps():
    return job()["steps"]


def code():
    return re.sub(r"(?m)^\s*#.*$", "", PATH.read_text())


def step(name_part):
    return next(s for s in steps() if name_part in s.get("name", ""))


def test_only_a_person_dispatching_it_starts_a_run_and_nothing_schedules_it():
    triggers = load().get("on", load().get(True))
    assert set(triggers) == {"workflow_dispatch"}
    assert "schedule" not in code() and "pull_request" not in code() and "\npush:" not in code()


def test_it_runs_only_from_main_by_the_owner_and_waits_for_approval():
    assert "github.ref == 'refs/heads/main'" in job()["if"]
    assert "github.actor == github.repository_owner" in job()["if"]
    assert job()["environment"] == "rotate-credentials"


def test_one_rotation_at_a_time_and_none_is_cancelled_part_way():
    c = load()["concurrency"]
    assert c["cancel-in-progress"] is False and c["group"] == "rotate-credentials"


def test_permissions_are_the_least_that_works():
    assert load()["permissions"] == {"contents": "read"}
    assert job()["permissions"] == {"contents": "read", "id-token": "write"}


def test_third_party_actions_are_pinned_to_a_commit():
    for s in steps():
        if "uses" in s:
            assert re.fullmatch(r"[\w./-]+@[0-9a-f]{40}", s["uses"]), s["uses"]


def test_inputs_are_checked_before_use_and_never_put_in_a_script_directly():
    assert "${{ inputs." not in re.sub(r"(?m)^\s*(INPUT_\w+|group):.*$", "", code())
    check = step("Check the inputs")["run"]
    for needle in ("INPUT_ENV", "INPUT_KIND", "INPUT_SETTLE"):
        assert needle in check
    assert "(all|certificates|credentials)" in check


def test_the_tool_is_built_from_this_commit_before_any_cloud_sign_in():
    names = [s.get("name", "") + s.get("uses", "") for s in steps()]
    build = next(i for i, n in enumerate(names) if "Build the rotation tool" in n)
    first_login = min(i for i, n in enumerate(names) if re.search(r"configure-aws-credentials|azure/login|google-github-actions/auth", n))
    assert build < first_login
    assert "--locked" in step("Build the rotation tool")["run"]


def test_no_cloud_secret_is_stored_and_only_the_known_secrets_are_read():
    used = set(re.findall(r"secrets\.(\w+)", code()))
    assert used <= IDENTIFYING | {"PLAN_STORE"}, used - IDENTIFYING
    assert "secrets.GITHUB_TOKEN" not in code()
    assert not re.search(r"AKIA|-----BEGIN|client_secret|access_key", code())


def test_values_that_identify_the_estate_are_secrets_never_variables():
    assert set(re.findall(r"vars\.(\w+)", code())) <= PLAIN_VARIABLES
    for name in IDENTIFYING:
        assert f"vars.{name}" not in code()


def test_every_tofu_and_manifest_command_goes_through_quiet():
    for s in steps():
        for line in s.get("run", "").splitlines():
            if re.search(r"\btofu\b", line) and not line.lstrip().startswith("#"):
                assert "scripts/quiet.sh" in line, line
            if "rotation_manifest.py" in line:
                assert "scripts/quiet.sh" in line, line


def test_the_workflow_never_reads_or_writes_a_secret_itself_only_the_tool_does():
    for forbidden in ("aws secretsmanager", "az keyvault secret", "gcloud secrets", "aws kms", "put-secret-value"):
        assert forbidden not in code(), forbidden
    assert "target/release/ledger-pki rotate" in step("Rotate")["run"]


def test_nothing_is_printed_but_counts_and_the_step_that_failed():
    printing = []
    for s in steps():
        # A group redirected into the job summary prints nothing to the log; it is checked below.
        text = re.sub(r'\{\n.*?\n\s*\} >> "\$GITHUB_STEP_SUMMARY"', "", s.get("run", ""), flags=re.S)
        for line in text.splitlines():
            line = line.strip()
            if re.match(r"(echo|printf|cat)\b", line) and not re.search(r">>?\s*\"?\$|> *\"|\| ?tee", line):
                printing.append(line)
    allowed = (
        'echo "::error::',
        'echo "::warning::',
        'echo "$line"',
    )
    for line in printing:
        assert line.startswith(allowed) or re.match(r"echo\s*$", line), line
    for forbidden in ("cat \"$RUNNER_TEMP", "manifest.json\" |", "outputs.json\" |", "jq ", "set -x", "set -o xtrace", "--debug", "--verbose"):
        assert forbidden not in code(), forbidden


def test_the_summary_holds_the_counts_line_the_mode_and_the_run_id_only():
    run = step("Rotate")["run"]
    summary = run[run.index("GITHUB_STEP_SUMMARY") - 200:]
    assert 'echo "$line"' in summary
    assert "INPUT_KIND" in summary and "GITHUB_RUN_ID" in summary
    for forbidden in ("manifest", "outputs", "secret", "address"):
        assert forbidden not in summary.lower().replace("github_step_summary", "")


def test_what_the_run_kept_on_the_runner_is_removed_even_after_a_failure():
    s = step("Remove what the run kept")
    assert s["if"] == "always()"
    for kept in ("manifest.json", "pki", "-outputs.json"):
        assert kept in s["run"]


def test_logs_are_kept_in_the_plan_store_before_the_vault_closes():
    names = [s.get("name", "") for s in steps()]
    assert names.index("Keep the logs in the plan store") < names.index("Close the Azure Key Vault again")
    assert step("Keep the logs")["if"] == "always()"


def test_the_key_vault_is_opened_for_the_run_only_and_closed_again_even_after_a_failure():
    opened, closed = step("Open the Azure Key Vault"), step("Close the Azure Key Vault")
    assert "network-rule add" in opened["run"] and "-o none" in opened["run"]
    assert "network-rule remove" in closed["run"] and "-o none" in closed["run"]
    assert closed["if"].startswith("always()")


def test_the_work_directory_is_on_the_runner_and_the_manifest_is_never_uploaded():
    assert "--work \"$RUNNER_TEMP/" in step("Rotate")["run"]
    assert "upload-artifact" not in code() and "actions/cache" not in code()


def test_the_runbook_and_the_trust_documentation_name_this_environment_exactly():
    doc = (ROOT / "docs" / "credentialed-workflows.md").read_text()
    assert "repo:UstDoesTech/OpenLedgerKernel:environment:rotate-credentials" in doc
    assert not re.search(r"environment:\*|repo:\*", doc)
    runbook = (ROOT / "docs" / "runbooks" / "rotating-credentials.md").read_text()
    assert "rotate-credentials" in runbook


def test_the_code_owners_cover_the_tool_and_its_script():
    owners = (ROOT / ".github" / "CODEOWNERS").read_text()
    for path in ("/crates/pki/", "/scripts/rotation_manifest.py", "/.github/"):
        assert re.search(rf"(?m)^{re.escape(path)}\s+@UstDoesTech$", owners), path


def test_proving_is_off_unless_a_person_turns_it_on_and_is_checked_before_use():
    proving = load()[True]["workflow_dispatch"]["inputs"]["proving"]
    assert proving["type"] == "boolean" and proving["default"] is False
    assert job()["env"]["INPUT_PROVING"] == "${{ inputs.proving }}"
    check = step("Check the inputs")["run"]
    assert "(true|false)" in check and "INPUT_PROVING" in check


def test_the_cells_layer_is_read_and_handed_to_the_manifest_only_for_a_proving_run():
    read = step("Read where the cells")["run"]
    assert re.search(r'\[ "\$INPUT_PROVING" = true \] && layers="\$layers cells"', read)
    assert re.search(r'\[ "\$INPUT_PROVING" = true \] && extra=\(', read)
    assert 'layers="api authority"' in read
    manifest = next(l for l in read.splitlines() if "rotation_manifest.py" in l)
    assert manifest.strip().startswith("scripts/quiet.sh manifest python3")
    assert "${extra[@]}" in manifest


def test_no_script_in_the_workflow_touches_an_administrator_credential_or_a_token():
    scripts = "\n".join(s.get("run", "") for s in steps()).lower()
    for word in ("password", "administrator", "tokens.json", "db.conn", "get-secret-value"):
        assert word not in scripts, "the tool handles these. The workflow's scripts must not touch them"
