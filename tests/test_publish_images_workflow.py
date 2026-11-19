"""The publish-images workflow keeps its safety properties.

These read .github/workflows/publish-images.yml as data. They do not run it.
"""

import re
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parent.parent
PATH = ROOT / ".github" / "workflows" / "publish-images.yml"
INFRA_APPLY = ROOT / ".github" / "workflows" / "infra-apply.yml"


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
    assert "github.actor == github.repository_owner" in job()["if"]


def test_it_waits_for_the_approved_environment_with_least_permissions():
    assert job()["environment"] == "infra-apply"
    assert job()["permissions"] == {"contents": "read", "id-token": "write", "packages": "write"}
    assert load()["permissions"] == {"contents": "read"}


def test_it_never_applies_infrastructure_and_holds_no_stored_credential():
    text = code()
    assert not re.search(r"\b(tofu|terraform)\s+(apply|destroy|import|refresh)\b", text)
    allowed = {"AWS_ROLE_ARN", "AZURE_CLIENT_ID", "AZURE_TENANT_ID", "AZURE_SUBSCRIPTION_ID",
               "GCP_WORKLOAD_IDENTITY_PROVIDER", "GCP_SERVICE_ACCOUNT", "TOFU_BACKEND_CONFIG"}
    assert set(re.findall(r"secrets\.(\w+)", text)) <= allowed, "the only token is the run's own (github.token)"
    for bad in ("AWS_SECRET_ACCESS_KEY", "AWS_ACCESS_KEY_ID", "client-secret", "credentials_json", "creds:", "az login", "docker login"):
        assert bad not in text, bad
    assert "upload-artifact" not in text


def test_third_party_actions_are_pinned_to_a_commit_like_the_apply_workflow():
    for s in steps():
        if "uses" in s:
            assert re.search(r"@[0-9a-f]{40}$", s["uses"]), s["uses"]
    pinned_in_apply = set(re.findall(r"uses:\s*(\S+@[0-9a-f]{40})", INFRA_APPLY.read_text()))
    used = {s["uses"] for s in steps() if "uses" in s}
    assert used <= pinned_in_apply, "use only the actions and commits already reviewed in infra-apply"


def test_the_image_comes_from_the_manifest_by_digest_and_every_copy_is_checked():
    text = code()
    assert "infra/images.json" in text
    run = named("Copy the image")["run"]
    # Copies keep the digest and go from the pinned digest, never from a tag.
    assert "--preserve-digests" in run and '"$SOURCE@$DIGEST"' in run and '"$GHCR@$DIGEST"' in run
    assert ":latest" not in text and "@latest" not in text
    # Upstream and every destination are checked against the pinned digest, and a changed tag fails.
    assert run.count("digest_of") >= 3
    assert "Tags cannot change" in run
    for dest in ('"$AWS_REPO"', '"$AZURE_REPO"', '"$GCP_REPO"'):
        assert dest in run


def test_the_azure_registry_is_opened_for_this_run_only_and_closed_again_even_after_a_failure():
    opener, closer = named("Open the Azure registry"), named("Close the Azure registry")
    order = [s.get("name") for s in steps()]
    assert order.index("Azure (federated)") < order.index(opener["name"]) < order.index(named("Copy the image")["name"])
    assert steps()[-1]['name'] == closer['name']
    assert "always()" in closer["if"] and "steps.registry.outputs.ip" in closer["if"]
    # One host, from this runner, never a range, written to the output before the rule is added.
    assert '"$ip/32"' in opener["run"] and '"$RUNNER_IP/32"' in closer["run"]
    assert "0.0.0.0" not in opener["run"] + closer["run"]
    assert opener["run"].index("GITHUB_OUTPUT") < opener["run"].index("network-rule add")
    assert "--default-action Deny" in opener["run"]
    assert "--public-network-enabled false" in closer["run"]
    # Inputs reach the shell through env, never by expression inside a script.
    assert "${{" not in opener["run"] + closer["run"] + named("Copy the image")["run"]


def test_tokens_are_masked_and_the_ghcr_package_is_the_repositorys_own():
    run = named("Copy the image")["run"]
    assert run.count("::add-mask::") == 2  # the registry tokens
    assert 'GHCR="ghcr.io/${GITHUB_REPOSITORY,,}/$IMAGE_NAME"' in run
    assert named("Copy the image")["env"]["GITHUB_TOKEN"] == "${{ github.token }}"


def test_the_inputs_are_checked_before_anything_else_runs():
    first = named("Check the inputs")["run"]
    assert "^[a-z][a-z0-9-]{0,30}$" in first
    assert "image must be etcd or postgres" in first
    order = [s.get("name") or s.get("uses") for s in steps()]
    assert order.index(named("Check the inputs")["name"]) < order.index("AWS (federated)")


def test_it_publishes_one_of_two_named_images_and_nothing_else():
    inputs = load().get("on", load().get(True))["workflow_dispatch"]["inputs"]
    assert inputs["image"]["type"] == "choice" and inputs["image"]["options"] == ["etcd", "postgres"]
    first = named("Check the inputs")["run"]
    # The layer and the output come from the choice, never from the input text.
    assert "etcd)     layer=registries; output=etcd_repositories" in first
    assert "postgres) layer=cell-first; output=postgres_repositories" in first
    assert 'output -json "$OUTPUT_NAME"' in named("Find the repositories")["run"]
    assert "${{" not in named("Find the repositories")["run"]


def test_postgres_comes_from_our_own_package_by_digest_and_is_not_copied_in_from_outside():
    first = named("Check the inputs")["run"]
    assert 'set(image) == {"source", "digest"}' in first
    assert '"ghcr.io/" + os.environ["GITHUB_REPOSITORY"].lower() + "/postgres"' in first
    run = named("Copy the image")["run"]
    assert 'if [ "$IMAGE_NAME" = etcd ]; then' in run
    assert '[ "$SOURCE" = "$GHCR" ]' in run
    # The only copy from outside is etcd's; every destination copy is from our package by digest.
    assert run.count('copy "$SOURCE@$DIGEST" "$GHCR"') == 1



def test_public_log_guards_registry_addresses_are_masked_and_skopeo_is_quiet():
    find = named("Find the repositories")["run"]
    assert "::add-mask::" in find
    assert "> /dev/null 2>&1" in find and "2> /dev/null" in find, "tofu output never reaches the log"
    for line in named("Copy the image")["run"].replace("\\\n", " ").splitlines():
        if re.match(r"\s*(printf .*\| )?skopeo (login|copy)\b", line.strip()):
            assert line.rstrip().endswith("> /dev/null"), line
