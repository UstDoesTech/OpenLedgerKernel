"""I-01kb: the self-hosted modules run the node image as a container. OpenTofu validates the
modules in CI; these tests hold what validation cannot: digest only, no public address, the
private endpoints, and an identity that reaches one secret and nothing else."""
import json
import re
import shutil
import subprocess
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent
MODULES = ROOT / "infra" / "modules"
POSTGRES = MODULES / "postgres"
CLOUDS = ["aws", "azure", "gcp"]
DIGEST = "@sha256:" + "0" * 64


def code(path: Path) -> str:
    return "\n".join(l for l in path.read_text().splitlines() if not l.lstrip().startswith("#"))


def body(directory: Path) -> str:
    return "\n".join(code(p) for p in sorted(directory.glob("*.tf")))


def form(cloud: str) -> str:
    return body(POSTGRES / f"{cloud}-self-hosted")


@pytest.mark.parametrize("cloud", CLOUDS)
def test_modules_take_an_image_by_digest_and_no_machine_image(cloud):
    text = form(cloud)
    variables = code(POSTGRES / f"{cloud}-self-hosted" / "variables.tf")
    assert 'variable "image"' in variables
    assert "@sha256:[0-9a-f]{64}$" in variables
    assert "Tags are refused" in variables
    assert not re.search(r"\bvar\.image_id\b|variable \"image_id\"", text)
    assert "var.image" in text


@pytest.mark.parametrize("cloud", CLOUDS)
def test_modules_use_the_shared_boot_configuration(cloud):
    text = form(cloud)
    assert 'source = "../../node-boot"' in text
    assert re.search(rf'cloud\s*=\s*"{cloud}"', text)
    assert "module.boot[count.index].ignition" in text


def test_facade_passes_the_image_and_describes_no_machine_image():
    text = body(POSTGRES)
    assert not re.search(r"(?<!system_)image_id", text)
    assert "var.aws.image" in text and "var.azure.image" in text and "var.gcp.image" in text
    assert "needs the node image, by digest" in text


@pytest.mark.parametrize("cloud", CLOUDS)
def test_node_config_names_the_secret_store_and_the_secret(cloud):
    text = form(cloud)
    kind = {"aws": "aws_secrets_manager", "azure": "azure_key_vault", "gcp": "gcp_secret_manager"}[cloud]
    assert f'secret_store              = {{ kind = "{kind}" }}' in text
    secret = {
        "aws": "aws_secretsmanager_secret.administrator.arn",
        "azure": "azurerm_key_vault_secret.administrator.versionless_id",
        "gcp": "google_secret_manager_secret.administrator.id",
    }[cloud]
    assert re.search(rf"secret_id\s*=\s*{re.escape(secret)}", text)


def test_aws_role_reaches_one_secret_and_its_key_and_one_repository():
    text = form("aws")
    secrets = re.search(r'statement \{\s*actions\s*=\s*\["secretsmanager:GetSecretValue", "secretsmanager:PutSecretValue"\]\s*resources\s*=\s*\[aws_secretsmanager_secret.administrator.arn\]', text)
    assert secrets
    assert text.count("secretsmanager:") == 3  # get and put on the administrator's secret, get on the node's certificates
    assert 'resources = ["*"]' in text and text.count('resources = ["*"]') == 1  # only the token call
    assert "ecr:GetAuthorizationToken" in text
    assert "resources = [var.registry_repository_arn]" in text
    assert not re.search(r"Action\W+\*|actions\s*=\s*\[\"\*\"", text)


def test_azure_identity_has_a_custom_two_action_role_on_the_one_secret():
    text = form("azure")
    block = re.search(r'resource "azurerm_role_definition" "secret" \{.*?\n\}', text, re.S).group(0)
    assert sorted(re.findall(r'"(Microsoft\.KeyVault/[^"]+)"', block)) == [
        "Microsoft.KeyVault/vaults/secrets/getSecret/action",
        "Microsoft.KeyVault/vaults/secrets/setSecret/action",
    ]
    # Every other role in the module is one read action, for the node's certificates.
    others = re.findall(r'"(Microsoft\.KeyVault/[^"]+)"', text.replace(block, ""))
    assert others == ["Microsoft.KeyVault/vaults/secrets/getSecret/action"]
    assert "scope              = azurerm_key_vault_secret.administrator.resource_versionless_id" in text
    assert "Key Vault Secrets Officer" not in text
    assert 'role_definition_name = "AcrPull"' in text
    assert "ignore_changes = [value]" in text  # the placeholder is never overwritten


def test_azure_placeholder_is_not_a_credential():
    text = form("azure")
    assert 'value        = "set-by-the-primary-on-first-start"' in text


def test_gcp_service_account_has_secret_level_roles_only():
    text = form("gcp")
    roles = re.findall(r'role\s*=\s*"(roles/[^"]+)"', text)
    assert sorted(roles) == [
        "roles/artifactregistry.reader",
        "roles/secretmanager.secretAccessor",  # the administrator's secret
        "roles/secretmanager.secretAccessor",  # the node's own certificates
        "roles/secretmanager.secretVersionAdder",
    ]
    assert "google_project_iam" not in text
    assert text.count("google_secret_manager_secret_iam_member") == 3
    assert "google_artifact_registry_repository_iam_member" in text


@pytest.mark.parametrize("cloud", CLOUDS)
def test_nothing_has_a_public_address_or_an_internet_route(cloud):
    text = form(cloud) + body(MODULES / f"private-access-{cloud}")
    assert not re.search(r"access_config|azurerm_public_ip|aws_eip|aws_nat_gateway|aws_internet_gateway|0\.0\.0\.0/0|associate_public_ip_address\s*=\s*true", text)
    assert not re.search(r"public_network_access_enabled\s*=\s*true", text)


def test_aws_reaches_registry_secret_store_and_layers_over_private_endpoints():
    text = body(MODULES / "private-access-aws")
    assert '["ecr.api", "ecr.dkr", "secretsmanager"]' in text
    assert "private_dns_enabled = true" in text
    assert re.search(r'vpc_endpoint_type\s*=\s*"Gateway"', text)  # image layers come from object storage
    assert text.count("aws_vpc_security_group_egress_rule") == 2  # to the endpoints and to storage
    assert "from_port                    = 443" in text
    assert "http_put_response_hop_limit = 2" in form("aws")
    assert 'http_tokens   = "required"' in form("aws")


def test_azure_reaches_registry_and_vault_over_private_endpoints_with_private_dns():
    text = body(MODULES / "private-access-azure")
    assert "privatelink.azurecr.io" in text and "privatelink.vaultcore.azure.net" in text
    assert 'subresource = "registry"' in text and 'subresource = "vault"' in text
    assert "azurerm_private_dns_zone_virtual_network_link" in text
    assert "is_manual_connection           = false" in text


def test_every_google_cloud_machine_boots_flatcar_without_secure_boot_and_keeps_the_rest():
    # Flatcar's public Google Cloud image is not signed for Google's Secure Boot keys, so with it
    # on the firmware refuses the disk and the machine never starts.
    for path in ["postgres/gcp-self-hosted", "api/gcp", "control-plane/gcp-members"]:
        text = body(MODULES / path)
        assert "enable_secure_boot          = false" in text, path
        assert "enable_vtpm                 = true" in text, path
        assert "enable_integrity_monitoring = true" in text, path
        assert "allow_stopping_for_update = true" in text, path


def test_azure_machines_may_send_https_to_the_endpoints_and_the_endpoints_accept_it():
    # The network denies everything in and out, so without these the machines time out
    # reaching the vault and the registry.
    text = body(MODULES / "private-access-azure")
    assert text.count('resource "azurerm_network_security_rule"') == 2
    assert text.count('destination_port_range       = "443"') == 2
    assert 'direction                    = "Outbound"' in text
    assert 'direction                    = "Inbound"' in text
    assert text.count('access                       = "Allow"') == 2
    assert "for_each = var.client_groups" in text
    assert "length(var.client_groups) > 0" in text, "a standalone deployment passes no groups and gets no rules"
    assert "var.endpoint_cidrs" in text
    assert "*" not in re.findall(r"destination_address_prefixes\s*=\s*(.*)", text)[0]
    assert "Internet" not in text and "0.0.0.0/0" not in text


def test_the_private_access_layer_gives_the_cells_the_api_and_the_control_plane_the_rules():
    text = (ROOT / "infra" / "envs" / "europe" / "private-access" / "main.tf").read_text()
    assert '["cell", "api", "control_plane"]' in text
    assert 'endpoint_cidrs                       = local.plan.azure.subnets["control_plane"]' in text


def test_gcp_reaches_google_apis_over_private_service_connect_with_private_dns():
    text = body(MODULES / "private-access-gcp")
    assert 'purpose      = "PRIVATE_SERVICE_CONNECT"' in text
    assert 'target                = "all-apis"' in text
    assert 'visibility  = "private"' in text
    assert "googleapis.com." in text and "pkg.dev." in text


@pytest.mark.parametrize("cloud", CLOUDS)
def test_registry_sign_in_uses_the_machines_own_identity_and_stores_nothing(cloud):
    script = (MODULES / "node-boot" / "files" / f"registry-login-{cloud}.sh").read_text()
    assert "docker login" in script and "--password-stdin" in script
    assert not re.search(r"--password[ =]\S|-p \S", script.replace("--password-stdin", ""))
    metadata = {"aws": "169.254.169.254", "azure": "169.254.169.254", "gcp": "metadata.google.internal"}[cloud]
    assert metadata in script


def test_boot_configuration_runs_the_container_locked_down_and_stops_os_updates():
    text = code(MODULES / "node-boot" / "main.tf")
    unit = (MODULES / "node-boot" / "files" / "ledger-postgres.service.tpl").read_text()
    for flag in ("--read-only", "--cap-drop ALL", "no-new-privileges", "--stop-timeout 60"):
        assert flag in unit
    assert "docker pull ${image}" in unit and unit.count("${image}") == 2  # the pull and the run
    assert "--privileged" not in unit and "--network host" not in unit
    assert 'name = "update-engine.service", mask = true' in text
    assert 'name = "locksmithd.service", mask = true' in text
    assert "/etc/ledger/tls:/run/secrets/tls:ro" in unit


def test_a_stopped_node_leaves_a_fixed_line_and_the_agents_own_lines_on_the_console():
    unit = (MODULES / "node-boot" / "files" / "ledger-postgres.service.tpl").read_text()
    line = next(l for l in unit.splitlines() if l.startswith("ExecStopPost="))
    assert "docker inspect --format" in line and "> /dev/console" in line
    assert "State.ExitCode" in line and "State.OOMKilled" in line
    # Only the agent's own lines, which begin "ledger-node:", may be taken from the container's log.
    assert unit.count("docker logs") == 1 and 'grep "^ledger-node:"' in line
    assert "docker logs ${name} 2>&1 | /usr/bin/grep" in line and "tail -n 5" in line
    assert "StandardOutput" not in unit and "StandardError" not in unit


def test_a_new_data_disk_has_its_lost_and_found_folder_removed_before_the_agent_sees_it():
    """mkfs.ext4 makes lost+found, and the node agent refuses a data directory that is not empty
    and holds no PostgreSQL data, so every new node stopped at once and restarted for ever."""
    script = (MODULES / "node-boot" / "files" / "prepare-data-disk.sh").read_text()
    mount = script.index("mount -o noatime")
    remove = script.index('rmdir "$target/lost+found"')
    assert mount < remove < script.index("chown 999:999")
    assert "rm -r" not in script  # only an empty folder may go, never data


@pytest.mark.skipif(not shutil.which("mkfs.ext4") or not shutil.which("debugfs"), reason="needs e2fsprogs")
def test_a_new_ext4_file_system_does_hold_a_lost_and_found_folder(tmp_path):
    image = tmp_path / "disk.img"
    image.write_bytes(b"\0" * 8 * 1024 * 1024)
    subprocess.run(["mkfs.ext4", "-q", "-F", "-L", "ledger-data", str(image)], check=True)
    listing = subprocess.run(["debugfs", "-R", "ls", str(image)], capture_output=True, text=True).stdout
    assert "lost+found" in listing


def test_boot_configuration_validation_refuses_a_tag():
    text = code(MODULES / "node-boot" / "variables.tf")
    pattern = re.search(r'regex\("([^"]+)", var\.image\)', text).group(1)
    pattern = pattern.replace("\\\\", "\\")
    ok = "registry.example:5000/repo/name" + DIGEST
    assert re.search(pattern, ok)
    assert not re.search(pattern, "registry.example/repo/name:latest")
    assert not re.search(pattern, "registry.example/repo/name:1.2" + DIGEST[:20])
    assert not re.search(pattern, "registry.example/repo/name@sha256:abc")


@pytest.mark.skipif(shutil.which("tofu") is None, reason="needs OpenTofu")
@pytest.mark.parametrize("cloud", CLOUDS)
def test_rendered_ignition_is_json_without_credentials(cloud, tmp_path):
    (tmp_path / "main.tf").write_text(f'''
module "b" {{
  source          = "{MODULES / 'node-boot'}"
  name            = "cell-n1"
  cloud           = "{cloud}"
  image           = "registry.example/ledger/node{DIGEST}"
  storage_gb      = 100
  node_config     = jsonencode({{ secret_store = {{ kind = "x" }} }})
  aws_region      = "eu-west-2"
  azure_tenant_id = "00000000-0000-0000-0000-000000000000"
  tls_secret      = "a-reference-to-a-secret"
  tls_files       = ["server.crt", "server.key", "ca.crt"]
}}
output "ignition" {{ value = module.b.ignition }}
''')
    run = lambda *a: subprocess.run(["tofu", f"-chdir={tmp_path}", *a], capture_output=True, text=True, check=True)
    run("init", "-input=false")
    run("apply", "-auto-approve", "-input=false")
    ignition = json.loads(json.loads(run("output", "-json", "ignition").stdout))
    assert ignition["ignition"]["version"] == "3.4.0"
    assert {f["path"] for f in ignition["storage"]["files"]} == {
        "/etc/ledger/node.json", "/opt/ledger/prepare-data-disk.sh", "/opt/ledger/registry-login.sh",
        "/opt/ledger/fetch-tls.sh", "/opt/ledger/write-tls.sh",
    }
    units = {u["name"]: u for u in ignition["systemd"]["units"]}
    assert units["update-engine.service"]["mask"] and units["locksmithd.service"]["mask"]
    unit = units["ledger-postgres.service"]["contents"]
    assert "registry.example/ledger/node" + DIGEST in unit
    assert "fetch-tls.sh a-reference-to-a-secret server.crt server.key ca.crt" in unit
    assert "BEGIN" not in json.dumps(ignition)  # no certificate or key content


def test_the_existing_postgres_module_tests_are_untouched():
    # No test in the file may be removed or renamed. A test's body may change only in a pull
    # request the owner reviews (it is labelled needs-human), as the AWS data disk change was.
    old = subprocess.run(
        ["git", "show", "origin/main:tests/test_postgres_module.py"],
        cwd=ROOT, capture_output=True, text=True,
    )
    if old.returncode == 0:
        names = lambda text: set(re.findall(r"^def (test_\w+)", text, re.M))
        now = (ROOT / "tests" / "test_postgres_module.py").read_text()
        assert names(old.stdout) <= names(now)


# I-01kc: certificates reach the nodes from the cloud's secret store.
@pytest.mark.parametrize("cloud", CLOUDS)
def test_each_node_has_its_own_certificate_secret_named_in_the_boot_configuration(cloud):
    text = form(cloud)
    secret = {"aws": "aws_secretsmanager_secret.tls", "azure": "azurerm_key_vault_secret.tls", "gcp": "google_secret_manager_secret.tls"}[cloud]
    assert re.search(rf"tls_secret\s*=\s*{re.escape(secret)}\[count\.index\]\.", text)
    assert re.search(rf'resource "{secret.split(".")[0]}" "tls" {{\s*count = 3', text)
    assert "local.tls_files.primary" in text and "local.tls_files.standby" in text


@pytest.mark.parametrize("cloud", CLOUDS)
def test_certificate_contents_never_pass_through_opentofu(cloud):
    text = form(cloud)
    assert "aws_secretsmanager_secret_version" not in text and "google_secret_manager_secret_version" not in text
    assert "secret_string" not in text and "secret_data" not in text
    if cloud == "azure":
        block = re.search(r'resource "azurerm_key_vault_secret" "tls" \{.*?\n\}', text, re.S).group(0)
        assert 'value        = "{}"' in block and "ignore_changes = [value]" in block


def test_each_node_reads_only_its_own_certificates():
    aws = form("aws")
    assert re.search(r'data "aws_iam_policy_document" "tls" \{\s*count = 3\s*statement \{\s*actions\s*=\s*\["secretsmanager:GetSecretValue"\]\s*resources\s*=\s*\[aws_secretsmanager_secret.tls\[count.index\].arn\]', aws)
    assert re.search(r'resource "aws_iam_role" "node" \{\s*count = 3', aws)  # a role for each node
    azure = form("azure")
    assert 'data_actions = ["Microsoft.KeyVault/vaults/secrets/getSecret/action"]' in azure
    assert "scope              = azurerm_key_vault_secret.tls[count.index].resource_versionless_id" in azure
    gcp = form("gcp")
    assert re.search(r'resource "google_service_account" "node" \{\s*count = 3', gcp)
    assert "secret_id = google_secret_manager_secret.tls[count.index].id" in gcp


def test_boot_configuration_validation_names_the_files_a_node_needs():
    text = code(MODULES / "node-boot" / "variables.tf")
    assert '"server.crt", "server.key", "ca.crt"' in text and "replication.key" in text


@pytest.mark.parametrize("cloud", CLOUDS)
def test_tls_fetch_uses_the_machines_identity_and_retries(cloud):
    script = (MODULES / "node-boot" / "files" / f"fetch-tls-{cloud}.sh").read_text()
    assert "write-tls.sh" in script and "seq 1 24" in script
    assert not re.search(r"(password|secret_key|client_secret)\s*=", script, re.I)
    assert {"aws": "latest/meta-data/iam", "azure": "metadata/identity/oauth2/token", "gcp": "service-accounts/default/token"}[cloud] in script


def test_tls_files_are_written_for_the_postgres_user_with_keys_private():
    script = (MODULES / "node-boot" / "files" / "write-tls.sh").read_text()
    assert "chmod 0400" in script and "chown 999:999" in script and "umask 077" in script
    assert "has($n)" in script  # a missing file stops the start


@pytest.mark.skipif(shutil.which("jq") is None, reason="needs jq")
def test_tls_writer_writes_the_files_and_refuses_a_missing_one(tmp_path):
    script = (MODULES / "node-boot" / "files" / "write-tls.sh").read_text()
    local = tmp_path / "write-tls.sh"
    local.write_text(script.replace("/etc/ledger/tls", str(tmp_path / "tls")).replace("chown 999:999", "true"))
    local.chmod(0o755)
    bundle = json.dumps({"server.crt": "c", "server.key": "k", "ca.crt": "a"})
    ok = subprocess.run([str(local), "server.crt", "server.key", "ca.crt"], input=bundle, text=True, capture_output=True)
    assert ok.returncode == 0, ok.stderr
    assert oct((tmp_path / "tls" / "server.key").stat().st_mode & 0o777) == "0o400"
    assert (tmp_path / "tls" / "ca.crt").read_text() == "a\n"
    bad = subprocess.run([str(local), "server.crt", "replication.key"], input=bundle, text=True, capture_output=True)
    assert bad.returncode != 0 and "replication.key" in bad.stderr
