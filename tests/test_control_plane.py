"""I-01e: the control plane module. `tofu test` runs placement and boot tests in CI
(infra/modules/control-plane/placement/tests, infra/modules/etcd-boot/tests); these tests hold
what those cannot: nothing public, certificates never in the repository or state, and each
member reading its own certificates and nothing else."""
import re
import shutil
import subprocess
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent
MODULES = ROOT / "infra" / "modules"
CP = MODULES / "control-plane"
CLOUDS = ["aws", "azure", "gcp"]


def code(path: Path) -> str:
    return "\n".join(l for l in path.read_text().splitlines() if not l.lstrip().startswith("#"))


def body(directory: Path) -> str:
    return "\n".join(code(p) for p in sorted(directory.glob("*.tf")))


@pytest.mark.parametrize("directory", [CP, CP / "placement", CP / "aws-members", CP / "azure-members", CP / "gcp-members", MODULES / "etcd-boot"])
def test_module_structure(directory):
    for name in ("versions.tf", "variables.tf", "outputs.tf", "main.tf"):
        assert (directory / name).exists(), f"{directory.name}/{name}"


def test_default_is_five_members_over_all_three_clouds_and_three_is_the_least():
    placement = body(CP / "placement")
    assert re.search(r'variable "members" \{[^}]*default\s*=\s*5', placement, re.S)
    assert 'default     = ["aws", "azure", "gcp"]' in placement
    assert "var.members >= 3" in placement
    assert "The control plane needs at least three members" in placement


def test_the_tofu_tests_cover_five_and_three_members_and_refuse_fewer():
    text = (CP / "placement" / "tests" / "placement.tftest.hcl").read_text()
    assert "aws = 2, azure = 2, gcp = 1" in text
    assert "aws = 1, azure = 1, gcp = 1" in text
    assert text.count("survives_any_single_loss") >= 4
    for count in ("members = 2", "members = 1", "members = 4"):
        block = text.split(count, 1)[1].split("run ", 1)[0]
        assert "expect_failures = [var.members]" in block


def test_ci_runs_the_tofu_tests():
    workflow = (ROOT / ".github" / "workflows" / "infra.yml").read_text()
    assert "*.tftest.hcl" in workflow and 'tofu -chdir="$dir" test' in workflow


@pytest.mark.skipif(shutil.which("tofu") is None, reason="needs OpenTofu")
@pytest.mark.parametrize("directory", [CP / "placement", MODULES / "etcd-boot"])
def test_tofu_tests_pass(directory, tmp_path):
    work = tmp_path / directory.name
    shutil.copytree(MODULES, tmp_path / "modules")
    work = tmp_path / "modules" / ("control-plane/placement" if directory.name == "placement" else "etcd-boot")
    run = lambda *a: subprocess.run(["tofu", f"-chdir={work}", *a], capture_output=True, text=True)
    assert run("init", "-backend=false", "-input=false").returncode == 0
    result = run("test")
    assert result.returncode == 0, result.stdout + result.stderr


def test_facade_calls_each_cloud_only_where_it_has_members_and_checks_inputs():
    text = body(CP)
    for cloud in CLOUDS:
        assert re.search(rf'module "{cloud}_members" \{{\s*source = "\./{cloud}-members"\s*count\s*=\s*length\(local\.in_cloud\.{cloud}\) > 0', text)
        assert f"var.{cloud} != null" in text
    assert 'source = "./placement"' in text


def test_facade_outputs_are_endpoints_members_quorum_survival_and_secret_references_only():
    text = code(CP / "outputs.tf")
    assert set(re.findall(r'^output "(\w+)"', text, re.M)) == {"endpoints", "members", "quorum", "survives_any_single_loss", "certificate_secrets"}


@pytest.mark.parametrize("cloud", CLOUDS)
def test_members_take_an_image_by_digest_and_run_through_the_shared_boot(cloud):
    variables = code(CP / f"{cloud}-members" / "variables.tf")
    assert "@sha256:[0-9a-f]{64}$" in variables and "Tags are refused" in variables
    text = body(CP / f"{cloud}-members")
    assert 'source   = "../../etcd-boot"' in text
    assert re.search(rf'cloud\s*=\s*"{cloud}"', text)


@pytest.mark.parametrize("cloud", CLOUDS)
def test_nothing_has_a_public_address_or_an_internet_route(cloud):
    text = body(CP / f"{cloud}-members")
    assert not re.search(r"access_config|azurerm_public_ip|aws_eip|aws_nat_gateway|aws_internet_gateway|0\.0\.0\.0/0|associate_public_ip_address\s*=\s*true|public_network_access_enabled\s*=\s*true", text)
    # The private endpoints and DNS zones belong to the private-access layer. A member module that
    # made its own would collide with them in the same network.
    assert "private-access-" not in text and 'module "private_access"' not in text
    if cloud == "aws":
        assert "associate_public_ip_address = false" in text and 'http_tokens   = "required"' in text


@pytest.mark.parametrize("cloud,secret", [("aws", "aws_secretsmanager_secret"), ("azure", "azurerm_key_vault_secret"), ("gcp", "google_secret_manager_secret")])
def test_each_member_has_its_own_certificate_secret_and_no_contents_in_opentofu(cloud, secret):
    text = body(CP / f"{cloud}-members")
    assert re.search(rf'resource "{secret}" "tls" \{{\s*for_each = local\.members', text)
    assert re.search(rf"tls_reference\s*=\s*{secret}\.tls\[each\.key\]\.", text)
    assert "secret_version" not in text and "secret_string" not in text and "secret_data" not in text
    if cloud == "azure":
        block = re.search(r'resource "azurerm_key_vault_secret" "tls" \{.*?\n\}', text, re.S).group(0)
        assert 'value        = "{}"' in block and "ignore_changes = [value]" in block


def test_each_member_reads_only_its_own_certificates():
    aws = body(CP / "aws-members")
    assert re.search(r'data "aws_iam_policy_document" "tls" \{\s*for_each = local\.members\s*statement \{\s*actions\s*=\s*\["secretsmanager:GetSecretValue"\]\s*resources\s*=\s*\[aws_secretsmanager_secret\.tls\[each\.key\]\.arn\]', aws)
    assert re.search(r'resource "aws_iam_role" "member" \{\s*for_each', aws)
    assert aws.count("secretsmanager:") == 1
    azure = body(CP / "azure-members")
    assert re.findall(r'"(Microsoft\.KeyVault/[^"]+)"', azure) == ["Microsoft.KeyVault/vaults/secrets/getSecret/action"]
    assert "scope              = azurerm_key_vault_secret.tls[each.key].resource_versionless_id" in azure
    gcp = body(CP / "gcp-members")
    assert re.search(r'resource "google_service_account" "member" \{\s*for_each', gcp)
    assert "secret_id = google_secret_manager_secret.tls[each.key].id" in gcp
    assert sorted(re.findall(r'role\s*=\s*"(roles/[^"]+)"', gcp)) == ["roles/artifactregistry.reader", "roles/secretmanager.secretAccessor"]
    assert "google_project_iam" not in gcp


def test_no_certificate_or_key_is_in_the_repository():
    for path in list(CP.rglob("*")) + list((MODULES / "etcd-boot").rglob("*")):
        if path.is_file():
            text = path.read_text()
            assert "BEGIN CERTIFICATE" not in text and "PRIVATE KEY" not in text, path


def test_etcd_boot_uses_mutual_tls_and_the_shared_scripts():
    text = code(MODULES / "etcd-boot" / "main.tf")
    for flag in ("--client-cert-auth", "--peer-client-cert-auth", "--tls-min-version TLS1.3"):
        assert flag in text
    assert "auto-tls" not in text and "http://" not in text
    assert "../node-boot/files" in text
    assert '"ca.crt", "server.crt", "server.key", "peer.crt", "peer.key"' in text


def test_the_flows_the_control_plane_needs_are_already_allowed_and_no_others():
    import json
    flows = json.loads((ROOT / "infra" / "flows.json").read_text())
    ports = flows["ports"]
    assert ports["etcd_client"] == 2379 and ports["etcd_peer"] == 2380
    etcd = {f["name"] for f in flows["flows"] if f["port"] in ("etcd_client", "etcd_peer")}
    assert etcd == {"api-to-control-plane-etcd", "cell-to-control-plane-etcd", "control-plane-peers"}


def test_an_etcd_member_says_why_it_failed_on_the_console_and_prints_no_secret():
    files = Path(__file__).resolve().parent.parent / "infra" / "modules" / "etcd-boot" / "files"
    for name in ("etcd-member.service.tpl", "etcd-data-disk.service.tpl"):
        text = (files / name).read_text()
        assert "StandardOutput=journal+console" in text and "StandardError=journal+console" in text, name
    unit = [l for l in (files / "etcd-member.service.tpl").read_text().splitlines() if not l.lstrip().startswith("#")]
    stop = next(l for l in unit if l.startswith("ExecStopPost="))
    assert "docker inspect --format" in stop and "> /dev/console" in stop
    assert "docker logs" not in stop and "key" not in stop.lower() and "cert" not in stop.lower()
