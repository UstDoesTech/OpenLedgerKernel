"""K-17: the manifest the rotation reads is built from the api and authority layers' outputs, is
kept private on the runner, and the script prints only a count."""
import json
import stat
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts" / "rotation_manifest.py"


def api():
    cell = lambda cloud, n: {
        "cell": f"acme-{cloud}-001", "address": f"10.0.{n}.5", "port": 8443, "internal_port": 8444,
        "secret_ref": f"secret-ref-{cloud}", "secret_files": ["db.conn"], "tls_terminates": "service",
    }
    return {"tenant": {"value": "ten_acme"},
            "apis": {"value": {"aws": cell("aws", 1), "azure": cell("azure", 2), "gcp": cell("gcp", 3)}}}


AUTHORITY = {"key_id": {"value": "arn-of-the-key"}, "certificate_secret": {"value": "arn-of-the-secret"}}


def run(tmp_path, api_outputs, authority=AUTHORITY):
    (tmp_path / "api.json").write_text(json.dumps(api_outputs))
    (tmp_path / "authority.json").write_text(json.dumps(authority))
    out = tmp_path / "manifest.json"
    done = subprocess.run(
        [sys.executable, str(SCRIPT), str(tmp_path / "api.json"), str(tmp_path / "authority.json"), str(out)],
        capture_output=True, text=True,
    )
    return done, out


def test_it_builds_one_entry_for_each_cell_from_the_layers_outputs(tmp_path):
    done, out = run(tmp_path, api())
    assert done.returncode == 0
    m = json.loads(out.read_text())
    assert m["tenants"] == ["ten_acme"]
    assert [c["cloud"] for c in m["cells"]] == ["aws", "azure", "gcp"]
    assert m["cells"][0] == {"name": "acme-aws-001", "cloud": "aws", "secret": "secret-ref-aws",
                             "address": "10.0.1.5", "internal_port": 8444}
    assert m["authority"] == {"key_id": "arn-of-the-key", "certificate_secret": "arn-of-the-secret"}


def test_the_manifest_is_readable_by_its_owner_only(tmp_path):
    _, out = run(tmp_path, api())
    assert stat.S_IMODE(out.stat().st_mode) == 0o600


def test_it_prints_only_a_count_and_nothing_it_was_given(tmp_path):
    done, _ = run(tmp_path, api())
    assert done.stdout.strip() == "cells: 3" and done.stderr == ""
    for given in ("acme", "secret-ref", "arn-of", "10.0."):
        assert given not in done.stdout + done.stderr


def test_a_failure_names_what_is_missing_and_never_a_value(tmp_path):
    bad = api()
    del bad["apis"]["value"]["gcp"]["secret_ref"]
    done, out = run(tmp_path, bad)
    assert done.returncode != 0 and not out.exists()
    assert "secret-ref" not in done.stdout + done.stderr and "acme" not in done.stdout + done.stderr


def test_a_tenant_that_is_not_an_identifier_is_refused(tmp_path):
    bad = api()
    bad["tenant"]["value"] = "acme"
    done, _ = run(tmp_path, bad)
    assert done.returncode != 0


def test_an_unknown_cloud_is_refused(tmp_path):
    bad = api()
    bad["apis"]["value"]["oracle"] = bad["apis"]["value"]["aws"]
    done, _ = run(tmp_path, bad)
    assert done.returncode != 0
