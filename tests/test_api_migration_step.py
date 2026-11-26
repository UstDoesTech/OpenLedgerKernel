"""K-25: the one-off migration step on the API machine. With the setting off there is no such unit and
the API's own unit is what it was; with it on the unit runs `ledger-api migrate` before the API and the
API will not start without it. Nothing secret appears in either."""
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BOOT = ROOT / "infra" / "modules" / "api-boot"
API = ROOT / "infra" / "modules" / "api"
ENV = ROOT / "infra" / "envs" / "europe" / "api"
MIGRATE = (BOOT / "files" / "ledger-api-migrate.service.tpl").read_text()
SERVICE = (BOOT / "files" / "ledger-api.service.tpl").read_text()
MAIN = (BOOT / "main.tf").read_text()


def lines(text):
    return [l for l in text.splitlines() if not l.lstrip().startswith("#")]


def test_the_setting_is_off_by_default_in_the_module_each_cloud_and_the_layer():
    for path in [BOOT / "variables.tf", *(API / c / "variables.tf" for c in ("aws", "azure", "gcp")), ENV / "variables.tf"]:
        block = re.search(r'variable "migrate_database" \{(.*?)\n\}', path.read_text(), re.S)
        assert block, path
        assert "type        = bool" in block.group(1) and "default     = false" in block.group(1), path


def test_the_layer_passes_the_setting_to_every_cloud_and_each_cloud_to_the_boot_module():
    env = (ENV / "main.tf").read_text()
    for c in ("aws", "azure", "gcp"):
        call = re.search(r'module "api_%s" \{(.*?)\n\}' % c, env, re.S).group(1)
        assert "migrate_database" in call and "var.migrate_database" in call, c
        boot = re.search(r'module "boot" \{(.*?)\n\}', (API / c / "main.tf").read_text(), re.S).group(1)
        assert re.search(r"migrate_database\s*=\s*var\.migrate_database", boot), c


def test_with_the_setting_off_there_is_no_unit_and_the_api_unit_has_no_extra_lines():
    assert "var.migrate_database ? [" in MAIN and "] : []" in MAIN
    assert re.search(r'migration_lines\s*=\s*var\.migrate_database \? ".*" : ""', MAIN)
    assert "${migration_lines}" in SERVICE


def test_with_it_on_the_api_requires_and_follows_the_migration_unit():
    assert "Requires=ledger-api-migrate.service" in MAIN and "After=ledger-api-migrate.service" in MAIN
    assert "Before=ledger-api.service" in MIGRATE


def test_the_unit_runs_the_same_image_with_migrate_and_only_the_database_settings():
    run = next(l for l in lines(MIGRATE) if l.startswith("ExecStart=/usr/bin/docker run"))
    assert run.rstrip().endswith("${image} migrate")
    assert "--publish" not in run and "--cap-drop ALL" in run and "--read-only" in run
    assert "--volume /etc/ledger-api/secrets:/run/secrets/api:ro" in run
    env = re.search(r"migrate_environment = \{(.*?)\n  \}", MAIN, re.S).group(1)
    assert set(re.findall(r"(LEDGER_\w+)\s*=", env)) == {"LEDGER_DB_FILE", "LEDGER_DB_CA_FILE"}


def test_a_failed_migration_is_retried_and_the_api_does_not_start_meanwhile():
    assert "Type=oneshot" in MIGRATE and "RemainAfterExit=yes" in MIGRATE
    assert "Restart=on-failure" in MIGRATE
    # `Requires=` stops the API if the unit fails; nothing in the API unit says to start it anyway.
    assert "Wants=ledger-api-migrate" not in MAIN + SERVICE


def test_the_unit_reaches_the_console_and_holds_nothing_secret():
    assert "StandardOutput=journal+console" in MIGRATE and "StandardError=journal+console" in MIGRATE
    assert not re.search(r"password|BEGIN|postgres://|sslkey", "\n".join(lines(MIGRATE)), re.I)


def test_the_runbook_exists_is_listed_and_names_the_setting():
    runbook = (ROOT / "docs" / "runbooks" / "applying-the-ledger-schema.md").read_text()
    assert "TF_VAR_migrate_database" in runbook and "ledger-api migrate" in runbook
    assert "applying-the-ledger-schema.md" in (ROOT / "docs" / "runbooks" / "README.md").read_text()
