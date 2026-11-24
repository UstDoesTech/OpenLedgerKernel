"""K-13: the API module. OpenTofu validates it in CI; these tests hold what validation cannot:
the machine is private, its identity reads one secret, nothing secret passes through OpenTofu,
and the service ends TLS itself."""
import json
import re
import shutil
import subprocess
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent
MODULES = ROOT / "infra" / "modules"
API = MODULES / "api"
BOOT = MODULES / "api-boot"
CLOUDS = ["aws", "azure", "gcp"]


def code(path: Path) -> str:
    return "\n".join(l for l in path.read_text().splitlines() if not l.lstrip().startswith("#"))


def body(directory: Path) -> str:
    return "\n".join(code(p) for p in sorted(directory.glob("*.tf")))


def test_there_is_one_form_for_each_cloud():
    assert sorted(p.name for p in API.iterdir() if p.is_dir()) == CLOUDS


@pytest.mark.parametrize("directory", [API / c for c in CLOUDS] + [BOOT])
def test_module_structure(directory):
    for name in ("versions.tf", "variables.tf", "outputs.tf", "main.tf"):
        assert (directory / name).exists(), f"{directory.name}/{name}"


@pytest.mark.parametrize("cloud", CLOUDS)
def test_the_image_is_taken_by_digest_only(cloud):
    text = body(API / cloud)
    assert re.search(r'variable "image"', text)
    assert "@sha256:[0-9a-f]{64}$" in text, "a tag is refused"
    assert re.search(r"image\s+= var\.image", text), "the image goes to the boot module as given"


def test_the_boot_module_refuses_a_tag():
    text = body(BOOT)
    assert "@sha256:[0-9a-f]{64}$" in text


@pytest.mark.parametrize("cloud", CLOUDS)
def test_outputs_are_references_and_never_a_secret(cloud):
    text = code(API / cloud / "outputs.tf")
    # The AWS machine also gives its id, so a load balancer can send it traffic (K-23b).
    expected = {"address", "secret_ref", "secret_files", "tls_terminates"} | ({"instance_id"} if cloud == "aws" else set())
    assert set(re.findall(r'^output "(\w+)"', text, re.M)) == expected


def test_nothing_secret_is_in_any_module_file():
    for tf in list(API.rglob("*.tf")) + list(BOOT.rglob("*.tf")):
        text = code(tf)
        assert not re.search(r"\bpassword\s*=", text, re.I), tf
        assert not re.search(r"postgres(ql)?://", text), tf
        assert "random_password" not in text, tf
        assert "BEGIN" not in text and "PRIVATE KEY" not in text, tf
        assert "aws_secretsmanager_secret_version" not in text, f"{tf} would put a value in state"
        assert "google_secret_manager_secret_version" not in text, f"{tf} would put a value in state"


def test_the_azure_secret_is_a_placeholder_whose_value_is_never_managed():
    text = body(API / "azure")
    assert 'value        = "{}"' in text
    assert "ignore_changes = [value]" in text


def test_aws_machine_is_private_and_reads_one_secret():
    text = body(API / "aws")
    assert "associate_public_ip_address = false" in text
    assert 'http_tokens   = "required"' in text
    assert "encrypted  = true" in text
    # The secret policy names the API secret and nothing broader.
    policy = re.search(r'data "aws_iam_policy_document" "secret" \{(.*?)\n\}\n', text, re.S).group(1)
    assert 'actions   = ["secretsmanager:GetSecretValue"]' in policy
    assert "resources = [aws_secretsmanager_secret.api.arn]" in policy
    assert "secretsmanager:PutSecretValue" not in text
    assert '"*"' not in policy
    # The only wildcard is the registry's sign-in call, which has no resource to name.
    assert text.count('"*"') == 1 and "ecr:GetAuthorizationToken" in text


def test_azure_machine_is_private_and_reads_one_secret():
    text = body(API / "azure")
    assert "azurerm_public_ip" not in text and "public_ip_address_id" not in text
    assert 'disable_password_authentication = true' in text
    assert "encryption_at_host_enabled      = true" in text
    assert 'data_actions = ["Microsoft.KeyVault/vaults/secrets/getSecret/action"]' in text
    assert "setSecret" not in text
    assert "scope              = azurerm_key_vault_secret.api.resource_versionless_id" in text
    assert 'role_definition_name = "AcrPull"' in text


def test_gcp_machine_is_private_and_reads_one_secret():
    text = body(API / "gcp")
    assert "access_config" not in text, "an access_config gives the machine a public address"
    assert "enable_secure_boot          = false" in text, "Flatcar's public image is not signed for Google's Secure Boot"
    assert "enable_vtpm                 = true" in text and "enable_integrity_monitoring = true" in text
    assert text.count('resource "google_secret_manager_secret_iam_member"') == 1
    assert 'role      = "roles/secretmanager.secretAccessor"' in text
    assert "secretVersionAdder" not in text and "secretmanager.admin" not in text
    assert 'role       = "roles/artifactregistry.reader"' in text


@pytest.mark.parametrize("cloud", CLOUDS)
def test_no_form_opens_anything_to_the_internet(cloud):
    text = body(API / cloud)
    assert "0.0.0.0/0" not in text
    assert "::/0" not in text


def test_the_service_ends_tls_itself_and_the_module_says_so():
    boot = body(BOOT)
    assert 'value       = "service"' in boot
    env = re.search(r"environment = \{(.*?)\n  \}", boot, re.S).group(1)
    assert "LEDGER_TLS_CERT_FILE" in env and "LEDGER_TLS_KEY_FILE" in env and "LEDGER_TLS_CA_FILE" in env
    assert "LEDGER_INTERNAL_AUTH_FILE" in env and "LEDGER_INTERNAL_LISTEN" in env
    # The retired variable would stop the service starting.
    assert "LEDGER_PEERS_FILE" not in boot
    # Every secret is a file the environment names, never a value in the environment.
    for name in re.findall(r"^\s*(LEDGER_\w+)\s*=", env, re.M):
        if name.endswith(("TOKENS", "PASSWORD", "SECRET", "KEY", "CONN")):
            pytest.fail(f"{name} looks like a secret value")


def test_etcd_must_be_https():
    text = code(BOOT / "variables.tf")
    assert "^https://" in text


def test_every_file_the_environment_names_is_in_the_secret():
    text = body(BOOT)
    def listed(name):
        found = re.search(rf"^  {name} = \[(.*?)\]", text, re.S | re.M)
        return set(found.group(1).replace('"', "").replace(",", " ").split())

    files = listed("secret_files")
    optional = listed("optional_secret_files")
    named = set(re.findall(r'\$\{local\.secrets_dir\}/([\w.-]+)"', text))
    assert named == files | optional
    assert not files & optional


def test_the_unit_runs_the_container_locked_down_and_pulls_by_digest():
    unit = (BOOT / "files" / "ledger-api.service.tpl").read_text()
    for flag in ("--read-only", "--cap-drop ALL", "--security-opt no-new-privileges"):
        assert flag in unit
    assert "--privileged" not in unit
    assert "docker pull ${image}" in unit
    assert "--volume /etc/ledger-api/secrets:/run/secrets/api:ro" in unit


def test_the_secret_writer_makes_files_readable_by_the_service_only():
    script = (BOOT / "files" / "write-secrets.sh").read_text()
    assert "umask 077" in script and "chmod 0400" in script
    assert 'owner="${LEDGER_API_SECRETS_OWNER:-10001:10001}"' in script and 'chown "$owner"' in script
    assert "exit 1" in script, "a missing entry stops the start"


@pytest.mark.skipif(shutil.which("tofu") is None, reason="OpenTofu is not installed")
def test_the_boot_module_renders_a_configuration_with_no_credential(tmp_path):
    work = tmp_path / "boot"
    shutil.copytree(BOOT, work)
    # The boot module reads the registry sign-in scripts from its sibling, so keep the layout.
    (tmp_path / "node-boot").symlink_to(MODULES / "node-boot")
    variables = {
        "name": "cell-api", "cell": "aws-test-01", "cloud": "aws",
        "image": "registry.example/ledger/api@sha256:" + "0" * 64,
        "secret": "arn:aws:secretsmanager:region:000000000000:secret:cell-api",
        "etcd_endpoints": ["https://10.0.0.1:2379"], "aws_region": "region",
    }
    (work / "t.tfvars.json").write_text(json.dumps(variables))
    run = lambda *a: subprocess.run(["tofu", f"-chdir={work}", *a], capture_output=True, text=True)
    assert run("init", "-backend=false", "-input=false").returncode == 0
    assert run("apply", "-auto-approve", "-input=false", "-var-file=t.tfvars.json").returncode == 0
    ignition = json.loads(json.loads(run("output", "-json", "ignition").stdout))
    unit = next(u for u in ignition["systemd"]["units"] if u["name"] == "ledger-api.service")
    assert "LEDGER_LISTEN=0.0.0.0:8443" in unit["contents"]
    assert "LEDGER_TLS_CERT_FILE=/run/secrets/api/server.crt" in unit["contents"]
    masked = {u["name"] for u in ignition["systemd"]["units"] if u.get("mask")}
    assert masked == {"update-engine.service", "locksmithd.service"}


def test_the_machine_reads_its_secret_again_so_a_rotation_reaches_the_service():
    """K-17: the secret is read at boot and then on a timer, with the same reader and the same
    entries. The service re-reads its files by itself, so nothing restarts."""
    service = (BOOT / "files" / "ledger-api-refresh.service.tpl").read_text()
    timer = (BOOT / "files" / "ledger-api-refresh.timer.tpl").read_text()
    assert "Type=oneshot" in service
    assert "ExecStart=/opt/ledger-api/fetch-secrets.sh ${secret_arguments}" in service
    commands = "\n".join(l for l in service.splitlines() if l.startswith("Exec"))
    for word in ("restart", "docker", "systemctl"):
        assert word not in commands, "a refresh never restarts the service"
    assert "OnUnitActiveSec=${every_minutes}min" in timer and "WantedBy=timers.target" in timer
    main = code(BOOT / "main.tf")
    assert re.search(r'name\s*=\s*"ledger-api-refresh.timer"', main)
    assert re.search(r"refresh_minutes\s*=\s*5\b", main), "the rotation job's wait depends on this"


def test_the_secret_writer_checks_every_entry_before_it_replaces_any_file():
    """K-17: a rotation that is missing an entry leaves the old files in force."""
    script = (BOOT / "files" / "write-secrets.sh").read_text()
    check = script.index('has($n)')
    first_write = script.index('mv "$target/$name.new"')
    assert check < first_write
    assert script.count("exit 1") >= 2


def _write(tmp_path, bundle, *names, existing=None):
    """Runs the real writer script as the current user, into a directory of its own."""
    import json
    import os
    import subprocess

    target = tmp_path / "secrets"
    target.mkdir(exist_ok=True)
    for name, text in (existing or {}).items():
        (target / name).write_text(text)
    env = dict(os.environ, LEDGER_API_SECRETS_DIR=str(target), LEDGER_API_SECRETS_OWNER=f"{os.getuid()}:{os.getgid()}")
    done = subprocess.run(
        ["bash", str(BOOT / "files" / "write-secrets.sh"), *names],
        input=json.dumps(bundle), text=True, capture_output=True, env=env,
    )
    return done, target


def test_an_optional_entry_is_written_when_the_secret_has_it(tmp_path):
    done, target = _write(tmp_path, {"a": "1", "db-ca.crt": "PEM"}, "a", "optional:db-ca.crt")
    assert done.returncode == 0, done.stderr
    assert (target / "db-ca.crt").read_text().strip() == "PEM"
    assert oct((target / "db-ca.crt").stat().st_mode & 0o777) == "0o400"


def test_a_secret_without_the_optional_entry_still_starts_and_a_stale_file_is_removed(tmp_path):
    done, target = _write(tmp_path, {"a": "1"}, "a", "optional:db-ca.crt", existing={"db-ca.crt": "OLD"})
    assert done.returncode == 0, done.stderr
    assert (target / "a").exists()
    assert not (target / "db-ca.crt").exists(), "the directory must match the secret"


def test_an_empty_optional_entry_counts_as_missing(tmp_path):
    done, target = _write(tmp_path, {"a": "1", "db-ca.crt": ""}, "a", "optional:db-ca.crt")
    assert done.returncode == 0, done.stderr
    assert not (target / "db-ca.crt").exists()


def test_a_missing_required_entry_still_stops_the_start_and_changes_nothing(tmp_path):
    done, target = _write(
        tmp_path, {"a": "1", "db-ca.crt": "NEW"}, "a", "b", "optional:db-ca.crt",
        existing={"a": "OLD", "db-ca.crt": "OLD"},
    )
    assert done.returncode != 0
    assert (target / "a").read_text() == "OLD"
    assert (target / "db-ca.crt").read_text() == "OLD"


def test_the_service_is_told_where_the_database_authority_is():
    text = body(BOOT)
    assert re.search(r'LEDGER_DB_CA_FILE\s*=\s*"\$\{local\.secrets_dir\}/db-ca\.crt"', text)
    assert "optional:" in text
