"""I-01i: what the Postgres node image must and must not contain. The container behaviour is
proved in CI by crates/node-agent/tests/containers.rs; these checks hold the build recipe."""
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DOCKERFILE = ROOT / "images" / "postgres" / "Dockerfile"
WORKFLOW = ROOT / ".github" / "workflows" / "image.yml"


def code(path: Path) -> str:
    return "\n".join(l for l in path.read_text().splitlines() if not l.lstrip().startswith("#"))


def test_the_runtime_is_the_official_postgres_16_image():
    froms = re.findall(r"^FROM\s+(\S+)", code(DOCKERFILE), re.M)
    assert froms[-1].startswith("postgres:16"), froms


def test_the_image_runs_as_a_named_non_root_user_and_stops_fast():
    text = code(DOCKERFILE)
    assert re.search(r"^USER\s+postgres\s*$", text, re.M)
    assert not re.search(r"^USER\s+(root|0)\b", text, re.M)
    assert re.search(r"^STOPSIGNAL\s+SIGINT\s*$", text, re.M)


def test_the_agent_is_the_entrypoint_so_postgres_becomes_the_main_process():
    assert 'ENTRYPOINT ["/usr/local/bin/ledger-node"]' in code(DOCKERFILE)


def test_nothing_secret_is_baked_in():
    text = code(DOCKERFILE)
    assert not re.search(r"^\s*(ENV|ARG)\s+\w*(PASSWORD|SECRET|TOKEN|KEY)\w*", text, re.M | re.I)
    # No key or certificate is copied in, with one exception: the public bundle of trusted
    # authorities that the secret stores' certificates are checked against.
    without_bundle = text.replace("/etc/ssl/certs/ca-certificates.crt", "")
    assert not re.search(r"^\s*COPY\s+.*\.(key|pem|crt)\b", without_bundle, re.M)
    assert "POSTGRES_HOST_AUTH_METHOD" not in text
    assert "trust" not in text


def test_no_extensions_are_added():
    text = code(DOCKERFILE)
    assert not re.search(r"apt(-get)?\s+install|CREATE EXTENSION|shared_preload_libraries|pglogical|timescale|postgis", text, re.I)


def test_the_binary_is_built_from_the_locked_dependencies():
    assert "cargo build --release --locked -p node-agent" in code(DOCKERFILE)


def test_the_workflow_builds_scans_and_proves_but_never_publishes():
    text = code(WORKFLOW)
    assert "scan_image_layers.py" in text
    assert "--test containers -- --ignored" in text
    assert not re.search(r"docker\s+(push|login)|login-action|secrets\.|id-token\s*:\s*write|packages\s*:\s*write", text)
    assert re.search(r"permissions:\s*\n\s*contents:\s*read", text)


def test_the_workflow_also_proves_the_customer_deployments():
    text = code(WORKFLOW)
    assert "kubectl kustomize deploy/kubernetes" in text
    assert "kubernetes-validate" in text
    assert "RENDERED_MANIFESTS" in text
    assert "deploy/compose/verify.sh" in text
    assert "compose.yaml down -v" in text


def test_the_agent_trusts_the_system_certificates_and_bundles_none():
    manifest = code(ROOT / "crates" / "node-agent" / "Cargo.toml")
    assert "native-tls-no-default" in manifest
    assert not re.search(r"webpki|rustls", manifest), "a bundled certificate list needs a licence deny.toml does not allow"
    assert not re.search(r"apt(-get)?\s+install", code(DOCKERFILE)), "the runtime OpenSSL and certificates come with the base image"


def test_the_image_carries_a_bundle_of_trusted_certificates_for_the_secret_stores():
    assert "COPY --from=build /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt" in code(DOCKERFILE)
