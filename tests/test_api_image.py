"""K-13: what the API image must and must not contain. The service's own behaviour is proved
by crates/api/tests; these checks hold the build recipe."""
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DOCKERFILE = ROOT / "images" / "api" / "Dockerfile"


def code(path: Path) -> str:
    return "\n".join(l for l in path.read_text().splitlines() if not l.lstrip().startswith("#"))


def test_it_builds_the_api_crate_from_this_repository_only():
    text = code(DOCKERFILE)
    assert "cargo build --release --locked -p api" in text
    assert "COPY crates crates" in text
    assert not re.search(r"\b(curl|wget|git clone)\b", text), "nothing is fetched into the image but packages"


def test_it_runs_as_a_named_non_root_user_with_the_binary_as_entrypoint():
    text = code(DOCKERFILE)
    assert re.search(r"^USER\s+10001\s*$", text, re.M)
    assert not re.search(r"^USER\s+(root|0)\b", text, re.M)
    assert 'ENTRYPOINT ["/usr/local/bin/ledger-api"]' in text


def test_it_serves_the_two_ports_the_module_publishes():
    text = code(DOCKERFILE)
    assert re.search(r"^EXPOSE\s+8443\s+8444\s*$", text, re.M)


def test_nothing_secret_is_baked_in():
    text = code(DOCKERFILE)
    assert not re.search(r"^\s*(ENV|ARG)\s+\w*(PASSWORD|SECRET|TOKEN|KEY)\w*", text, re.M | re.I)
    assert not re.search(r"^\s*COPY\s+.*\.(key|pem|crt|json)\b", text, re.M)
    assert not re.search(r"^\s*ENV\s+LEDGER_", text, re.M), "configuration is given when it starts, not baked in"


def test_every_variable_the_readme_names_is_one_the_service_reads():
    readme = (ROOT / "images" / "api" / "README.md").read_text()
    service = (ROOT / "crates" / "api" / "src" / "main.rs").read_text()
    names = set(re.findall(r"\bLEDGER_[A-Z_]+\b", readme))
    assert names, "the README lists the variables"
    for name in names:
        assert name in service, name


def test_the_build_stage_has_the_protobuf_compiler_and_the_runtime_stage_does_not():
    build, runtime = code(DOCKERFILE).split("FROM debian:bookworm-slim")
    assert "protobuf-compiler" in build, "etcd-client needs protoc to build"
    assert "protobuf-compiler" not in runtime
