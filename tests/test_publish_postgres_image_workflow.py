"""The publish-postgres-image workflow (I-01t) keeps its safety properties.

These read .github/workflows/publish-postgres-image.yml as data. They do not run it.
"""

import re
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parent.parent
PATH = ROOT / ".github" / "workflows" / "publish-postgres-image.yml"


def load():
    return yaml.safe_load(PATH.read_text())


def job():
    return load()["jobs"]["publish"]


def steps():
    return job()["steps"]


def code():
    return re.sub(r"(?m)^\s*#.*$", "", PATH.read_text())


def named(prefix):
    return next(s for s in steps() if s.get("name", "").startswith(prefix))


def test_only_a_person_dispatching_it_starts_a_run_and_only_from_main():
    wf = load()
    assert set(wf.get("on", wf.get(True))) == {"workflow_dispatch"}
    assert "github.ref == 'refs/heads/main'" in job()["if"]


def test_it_uses_least_permissions_and_no_cloud_access():
    assert job()["permissions"] == {"contents": "read", "packages": "write"}
    assert load()["permissions"] == {"contents": "read"}
    assert "environment" not in job()
    text = code()
    for bad in ("id-token", "aws-actions", "azure/login", "google-github-actions", "secrets.", "tofu", "terraform"):
        assert bad not in text, bad
    for bad in ("AWS_SECRET_ACCESS_KEY", "AWS_ACCESS_KEY_ID", "client-secret", "credentials_json"):
        assert bad not in text, bad


def test_third_party_actions_are_pinned_to_full_commits():
    for s in steps():
        if "uses" in s:
            assert re.search(r"@[0-9a-f]{40}$", s["uses"]), s["uses"]


def test_it_scans_the_layers_for_secrets_before_it_pushes():
    names = [s.get("name", "") for s in steps()]
    scan = next(i for i, n in enumerate(names) if n.startswith("Scan"))
    push = next(i for i, n in enumerate(names) if n.startswith("Push"))
    assert scan < push
    assert "scripts/scan_image_layers.py" in named("Scan")["run"]
    assert "images/postgres/Dockerfile" in named("Build")["run"]


def test_the_push_uses_the_runs_own_token_and_checks_the_digest():
    run = named("Push")["run"]
    assert named("Push")["env"] == {"GITHUB_TOKEN": "${{ github.token }}"}
    assert "--password-stdin" in run and "add-mask" in run
    assert "ghcr.io/${GITHUB_REPOSITORY,,}/postgres" in run
    assert "sha256:[0-9a-f]{64}" in run
    assert "imagetools inspect" in run and '[ "$registry" = "$pushed" ]' in run
    assert "docker logout ghcr.io" in run
    assert "git-${GITHUB_SHA}" in run


def test_the_image_is_linked_to_this_repository_and_the_digest_is_reported():
    assert "org.opencontainers.image.source=https://github.com/${GITHUB_REPOSITORY}" in named("Build")["run"]
    assert "GITHUB_STEP_SUMMARY" in named("Push")["run"]
    assert "latest" not in re.sub(r"(?m)^\s*#.*$", "", PATH.read_text()).replace("ubuntu-latest", "")
