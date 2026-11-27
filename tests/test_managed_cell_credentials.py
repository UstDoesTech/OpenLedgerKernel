"""K-24d: rotate-credentials writes a managed cell's connection and says how the service signs in.
The run itself is proven in crates/pki/tests/managed_cell_credentials.rs; these hold the manifest
builder and what the code and the docs say."""
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts" / "rotation_manifest.py"
PEM = "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n"


def api():
    cell = lambda cloud, n: {
        "cell": f"acme-{cloud}-001", "address": f"10.0.{n}.5", "port": 8443, "internal_port": 8444,
        "secret_ref": f"secret-ref-{cloud}", "secret_files": ["db.conn"], "tls_terminates": "service",
    }
    return {"tenant": {"value": "ten_acme"},
            "apis": {"value": {"aws": cell("aws", 1), "azure": cell("azure", 2), "gcp": cell("gcp", 3)}}}


def cells(kind):
    one = lambda cloud: {"kind": kind, "host": f"db-{cloud}.cells.example", "port": 5432,
                         "secret_ref": f"administrator-ref-{cloud}"}
    return {"tenant": {"value": "ten_acme"},
            "cells": {"value": {c: one(c) for c in ("aws", "azure", "gcp")}}}


def build(tmp_path, kind, *extra):
    files = {"api.json": api(), "authority.json": {"key_id": {"value": "k"}, "certificate_secret": {"value": "s"}},
             "cells.json": cells(kind)}
    for name, body in files.items():
        (tmp_path / name).write_text(json.dumps(body))
    out = tmp_path / "manifest.json"
    done = subprocess.run(
        [sys.executable, str(SCRIPT), *(str(tmp_path / n) for n in ("api.json", "authority.json")), str(out),
         str(tmp_path / "cells.json"), *extra],
        capture_output=True, text=True,
    )
    return done, out


def test_a_managed_cell_says_how_the_service_signs_in_by_its_cloud(tmp_path):
    done, out = build(tmp_path, "managed")
    assert done.returncode == 0, done.stderr
    got = {c["cloud"]: c["database"]["sign_in"] for c in json.loads(out.read_text())["cells"]}
    assert got == {"aws": "aws_iam", "azure": "azure_entra", "gcp": "gcp_iam"}
    assert "password" not in out.read_text().lower()


def test_a_self_hosted_cell_has_no_sign_in_so_it_is_unchanged(tmp_path):
    done, out = build(tmp_path, "self_hosted")
    assert done.returncode == 0, done.stderr
    assert all("sign_in" not in c["database"] and "db_ca" not in c for c in json.loads(out.read_text())["cells"])


def test_a_db_ca_file_goes_to_that_clouds_managed_cell_only(tmp_path):
    (tmp_path / "aws.pem").write_text(PEM)
    done, out = build(tmp_path, "managed", "--db-ca", f"aws={tmp_path / 'aws.pem'}")
    assert done.returncode == 0, done.stderr
    by = {c["cloud"]: c for c in json.loads(out.read_text())["cells"]}
    assert by["aws"]["db_ca"] == PEM and "db_ca" not in by["azure"] and "db_ca" not in by["gcp"]
    done, out = build(tmp_path, "self_hosted", "--db-ca", f"aws={tmp_path / 'aws.pem'}")
    assert all("db_ca" not in c for c in json.loads(out.read_text())["cells"])


def test_a_bad_db_ca_is_refused_without_its_contents(tmp_path):
    (tmp_path / "bad.pem").write_text("secret-looking-text")
    for arg in (f"aws={tmp_path / 'bad.pem'}", f"mars={tmp_path / 'bad.pem'}", "aws"):
        done, _ = build(tmp_path, "managed", "--db-ca", arg)
        assert done.returncode != 0
        assert "secret-looking-text" not in done.stdout + done.stderr


def code(path):
    return "\n".join(l for l in path.read_text().splitlines() if not l.lstrip().startswith("//"))


def test_the_managed_block_has_no_source_for_the_administrators_secret():
    rotate = (ROOT / "crates" / "pki" / "src" / "rotate.rs").read_text()
    block = rotate.split("// A managed cell's connection.", 1)[1].split("// A proving environment's missing entries", 1)[0]
    block = "\n".join(l for l in block.splitlines() if not l.lstrip().startswith("//"))
    for banned in ("credential_secret", "source.", "AdministratorCopy", "self.store.read"):
        assert banned not in block, banned
    assert "identity_connection" in block


def test_identity_connection_names_no_password():
    text = code(ROOT / "crates" / "pki" / "src" / "database.rs")
    fn = text.split("pub fn identity_connection", 1)[1].split("\n}\n", 1)[0]
    assert "password" not in fn and "sslmode=require" in fn


def test_the_doc_says_what_a_managed_cell_gets_and_what_is_left():
    doc = (ROOT / "docs" / "credentialed-workflows.md").read_text()
    section = doc.split("#### Managed cells (K-24d)", 1)[1].split("\n#", 1)[0]
    for word in ("db.conn", "db-sign-in", "db_ca", "--db-ca", "administrator"):
        assert word in section, word
    assert re.search(r"no password", section)
