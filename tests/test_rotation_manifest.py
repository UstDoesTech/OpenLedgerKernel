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


def cells(kind="self_hosted"):
    one = lambda cloud, n: {"kind": kind, "host": f"db-{cloud}.cells.example", "port": 5432,
                            "secret_ref": f"administrator-ref-{cloud}"}
    return {"tenant": {"value": "ten_acme"},
            "cells": {"value": {"aws": one("aws", 1), "azure": one("azure", 2), "gcp": one("gcp", 3)}}}


def run_proving(tmp_path, cells_outputs, proving=True):
    (tmp_path / "cells.json").write_text(json.dumps(cells_outputs))
    (tmp_path / "api.json").write_text(json.dumps(api()))
    (tmp_path / "authority.json").write_text(json.dumps(AUTHORITY))
    out = tmp_path / "manifest.json"
    done = subprocess.run(
        [sys.executable, str(SCRIPT), str(tmp_path / "api.json"), str(tmp_path / "authority.json"),
         str(out), str(tmp_path / "cells.json"), *(["proving"] if proving else [])],
        capture_output=True, text=True,
    )
    return done, out


def test_without_the_cells_outputs_the_manifest_is_not_a_proving_one(tmp_path):
    done, out = run(tmp_path, api())
    assert done.returncode == 0, done.stderr
    manifest = json.loads(out.read_text())
    assert "proving" not in manifest
    assert all("database" not in c for c in manifest["cells"])


def test_with_the_cells_outputs_each_cell_carries_its_own_database_and_where_its_credential_is(tmp_path):
    done, out = run_proving(tmp_path, cells())
    assert done.returncode == 0, done.stderr
    manifest = json.loads(out.read_text())
    assert manifest["proving"] is True
    for c in manifest["cells"]:
        cloud = c["cloud"]
        assert c["database"] == {"host": f"db-{cloud}.cells.example", "port": 5432,
                                 "kind": "self_hosted", "credential_secret": f"administrator-ref-{cloud}",
                                 "nodes": []}
    # The reference is there. A credential never is: the script cannot read one.
    assert "password" not in out.read_text().lower()
    assert stat.S_IMODE(out.stat().st_mode) == 0o600
    assert done.stdout.strip() == "cells: 3"


def test_a_proving_manifest_is_refused_when_a_database_is_not_plain_or_is_missing(tmp_path):
    bad = cells()
    bad["cells"]["value"]["aws"]["host"] = "db host; rm -rf"
    done, _ = run_proving(tmp_path, bad)
    assert done.returncode != 0 and "db host" not in done.stderr + done.stdout

    short = cells()
    del short["cells"]["value"]["gcp"]
    done, _ = run_proving(tmp_path, short)
    assert done.returncode != 0
    assert "administrator-ref" not in done.stderr + done.stdout

    odd = cells(kind="somewhere_else")
    done, _ = run_proving(tmp_path, odd)
    assert done.returncode != 0


def with_nodes(cloud="aws"):
    out = cells()
    out["cells"]["value"][cloud]["nodes"] = [
        {"name": f"{cloud}-n{i}", "role": "primary" if i == 1 else "standby",
         "address": f"10.0.{i}.10", "secret_ref": f"tls-ref-{cloud}-{i}"}
        for i in (1, 2, 3)
    ]
    return out


def test_the_cells_outputs_without_the_word_proving_give_databases_and_nodes_but_not_a_proving_manifest(tmp_path):
    done, out = run_proving(tmp_path, with_nodes(), proving=False)
    assert done.returncode == 0, done.stderr
    manifest = json.loads(out.read_text())
    assert "proving" not in manifest
    aws = next(c for c in manifest["cells"] if c["cloud"] == "aws")
    assert [n["role"] for n in aws["database"]["nodes"]] == ["primary", "standby", "standby"]
    assert aws["database"]["nodes"][1] == {"name": "aws-n2", "role": "standby", "address": "10.0.2.10",
                                           "secret": "tls-ref-aws-2"}
    other = next(c for c in manifest["cells"] if c["cloud"] == "gcp")
    assert other["database"]["nodes"] == []
    assert "tls-ref" not in done.stdout + done.stderr and done.stdout.strip() == "cells: 3"


def test_a_machine_that_is_not_plain_or_a_cell_someone_else_runs_with_machines_is_refused(tmp_path):
    for mutate in (
        lambda o: o["cells"]["value"]["aws"]["nodes"][0].update(role="leader"),
        lambda o: o["cells"]["value"]["aws"]["nodes"][0].update(address="10.0.0.1; x"),
        lambda o: o["cells"]["value"]["aws"]["nodes"][0].update(secret_ref=""),
        lambda o: o["cells"]["value"]["aws"].update(kind="managed"),
    ):
        bad = with_nodes()
        mutate(bad)
        done, _ = run_proving(tmp_path, bad, proving=False)
        assert done.returncode != 0
        assert "tls-ref" not in done.stderr + done.stdout and "10.0." not in done.stderr + done.stdout
