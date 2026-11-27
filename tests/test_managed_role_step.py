"""K-24cc: an API machine makes its managed cell's application role, once. A managed database has a
private address only, so a hosted runner cannot do it. These hold the boot module, the AWS grant and
the layer; the command itself is proven in crates/api/tests/binary.rs and crates/pki/tests/application_role.rs."""
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BOOT = ROOT / "infra" / "modules" / "api-boot"
API_AWS = ROOT / "infra" / "modules" / "api" / "aws"
LAYER = ROOT / "infra" / "envs" / "europe" / "api"


def tf(path):
    return "\n".join(l for l in path.read_text().splitlines() if not l.lstrip().startswith("#"))


def test_the_step_is_off_by_default_at_every_level():
    for path, name in ((BOOT, "create_database_role"), (API_AWS, "create_database_role"), (LAYER, "create_database_role")):
        text = tf(path / "variables.tf")
        block = re.search(rf'variable "{name}" \{{(.*?)\n\}}', text, re.S).group(1)
        assert re.search(r"default\s+= false", block), path
    # Azure and Google Cloud do not take it, so their machines cannot get the step.
    for cloud in ("azure", "gcp"):
        assert "create_database_role" not in (ROOT / "infra" / "modules" / "api" / cloud / "variables.tf").read_text()


def test_the_unit_and_the_fetch_script_exist_only_when_the_step_is_on():
    main = tf(BOOT / "main.tf")
    assert re.search(r"units = concat\(var\.create_database_role \? \[\s*\{\s*name\s+= \"ledger-api-managed-role\.service\"", main)
    assert 'var.create_database_role ? [\n        { path = "/opt/ledger-api/fetch-administrator.sh"' in main.replace("\r", "") or \
        re.search(r'var\.create_database_role \? \[\s*\{ path = "/opt/ledger-api/fetch-administrator\.sh"', main)
    assert re.search(r'fetch_administrator = var\.create_database_role \? file\(', main)
    assert 'role_lines = var.create_database_role ? "Requires=ledger-api-managed-role.service' in main


def test_only_aws_can_have_it_and_it_needs_the_secret_reference():
    v = tf(BOOT / "variables.tf")
    assert 'condition     = !var.create_database_role || var.cloud == "aws"' in v
    assert 'condition     = !var.create_database_role || var.administrator_secret != ""' in v
    a = tf(API_AWS / "variables.tf")
    assert "!var.create_database_role || (var.administrator_secret_arn != null && var.database_resource_id != null)" in a


def test_the_machines_role_may_read_the_administrators_secret_only_while_the_step_is_on_and_only_that_secret():
    text = tf(API_AWS / "main.tf")
    doc = re.search(r'data "aws_iam_policy_document" "administrator" \{(.*?)\n\}', text, re.S).group(1)
    assert "count = var.create_database_role ? 1 : 0" in doc
    assert doc.count("statement {") == 1
    assert re.search(r'actions\s+= \["secretsmanager:GetSecretValue"\]', doc)
    assert re.search(r"resources\s+= \[var\.administrator_secret_arn\]", doc) and "*" not in doc
    res = re.search(r'resource "aws_iam_role_policy" "administrator" \{(.*?)\n\}', text, re.S).group(1)
    assert "count = var.create_database_role ? 1 : 0" in res and "aws_iam_role.api.id" in res
    # No other policy of the machine names the administrator's secret.
    others = text.replace(doc, "").replace(res, "")
    assert "administrator_secret_arn" not in others.replace("coalesce(var.administrator_secret_arn", "")


def test_the_secret_reference_is_never_an_output_and_the_credential_never_a_value():
    assert "administrator" not in tf(API_AWS / "outputs.tf") and "administrator" not in tf(BOOT / "outputs.tf")
    for path in list(BOOT.rglob("*.tf")) + list(API_AWS.glob("*.tf")) + list(LAYER.glob("*.tf")):
        assert not re.search(r"password\s*=", tf(path)), path


def test_the_unit_gives_the_administrators_file_to_its_own_container_only_and_removes_it_on_success_and_failure():
    unit = (BOOT / "files" / "ledger-api-managed-role.service.tpl").read_text()
    assert "--volume /run/ledger-administrator:/run/administrator:ro" in unit
    assert unit.rstrip().count("ExecStartPost=/usr/bin/rm -rf /run/ledger-administrator") == 1
    # Once it has worked it is skipped, so a reboot after the grant is gone cannot stop the API.
    assert "ConditionPathExists=!/etc/ledger-api/application-role-made" in unit
    assert unit.index("rm -rf /run/ledger-administrator") < unit.index("touch /etc/ledger-api/application-role-made")
    assert "ExecStopPost=/usr/bin/rm -rf /run/ledger-administrator" in unit
    assert unit.index("fetch-administrator.sh") < unit.index("ExecStart=/usr/bin/docker run")
    assert re.search(r"docker run .* \$\{image\} managed-role\n", unit)
    for other in ("ledger-api.service.tpl", "ledger-api-migrate.service.tpl", "ledger-api-refresh.service.tpl"):
        text = (BOOT / "files" / other).read_text()
        assert "ledger-administrator" not in text and "administrator" not in text.lower().replace("administrator's", ""), other


def test_the_fetch_script_writes_to_memory_only_keeps_it_private_and_prints_nothing_secret():
    script = (BOOT / "files" / "fetch-administrator-aws.sh").read_text()
    assert "dir=/run/ledger-administrator" in script and "umask 077" in script
    assert "/etc/ledger-api/secrets" in script and 'secrets/db.conn' in script  # reads host and port only
    assert "> /etc/ledger-api" not in script and "write-secrets.sh" not in script
    # The only things it prints are fixed phrases, to the error stream: no echo quotes a variable.
    for line in script.splitlines():
        if re.match(r"\s*(echo|printf)\b", line) or "|| { echo" in line or "&& { echo" in line:
            assert ">&2" in line, line
            for quoted in re.findall(r'echo "([^"]*)"', line):
                assert "$" not in quoted.replace("$attempt", ""), line  # the attempt number is a count
    assert "here-document" in script and "<<CONNECTION" in script
    assert "set -x" not in script and "xtrace" not in script


def test_the_schema_step_and_the_api_wait_for_the_role_step_when_it_is_on():
    main = tf(BOOT / "main.tf")
    assert "role_lines            = local.role_lines" in main
    assert "${role_lines}" in (BOOT / "files" / "ledger-api-migrate.service.tpl").read_text()
    assert 'migration_lines       = "${local.migration_lines}${local.role_lines}"' in main  # the API unit's own lines
    unit = (BOOT / "files" / "ledger-api-managed-role.service.tpl").read_text()
    assert "Before=ledger-api-migrate.service ledger-api.service" in unit


def test_the_layer_names_the_cells_secret_only_for_a_managed_aws_cell():
    layer = tf(LAYER / "main.tf")
    aws = re.search(r'module "api_aws" \{(.*?)\n\}', layer, re.S).group(1)
    assert "create_database_role      = var.create_database_role" in aws
    assert 'local.cells["aws"].kind == "managed" ? local.cells["aws"].secret_ref : null' in aws
    for cloud in ("azure", "gcp"):
        assert "create_database_role" not in re.search(rf'module "api_{cloud}" \{{(.*?)\n\}}', layer, re.S).group(1)


def test_the_runbook_says_when_to_turn_it_on_and_off_and_what_it_costs():
    doc = (ROOT / "docs" / "runbooks" / "applying-the-ledger-schema.md").read_text()
    section = doc.split("## Making a managed cell's application role", 1)[1].split("\n## ", 1)[0]
    for word in ("TF_VAR_create_database_role", "administrator", "off again", "replace"):
        assert word in section, word
