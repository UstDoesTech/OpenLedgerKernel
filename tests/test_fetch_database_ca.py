"""K-24f: the script that fetches a managed database's public CA bundle. No network: the download is
replaced by a function that returns what the test says."""
import base64
import importlib.util
import json
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("fetch_database_ca", ROOT / "scripts" / "fetch_database_ca.py")
ca = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ca)

DER = b"\x30\x03\x02\x01\x01"
GOOD = "-----BEGIN CERTIFICATE-----\n" + base64.b64encode(DER).decode() + "\n-----END CERTIFICATE-----\n"


def cells(tmp_path, **kinds):
    path = tmp_path / "cells.json"
    path.write_text(json.dumps({"cells": {"value": {c: {"kind": k} for c, k in kinds.items()}}}))
    return str(path)


def test_it_writes_a_bundle_for_each_managed_cell_and_nothing_for_a_self_hosted_one(tmp_path, monkeypatch, capsys):
    monkeypatch.setattr(ca, "fetch", lambda url: GOOD * 2)
    out = tmp_path / "out"
    assert ca.main(["x", cells(tmp_path, aws="managed", azure="self_hosted", gcp="self_hosted"), str(out)]) == 0
    assert sorted(p.name for p in out.iterdir()) == ["aws.pem"]
    assert (out / "aws.pem").read_text() == GOOD * 2
    assert capsys.readouterr().out.strip() == "database CA bundles: 1"


def test_a_run_with_no_managed_cell_fetches_nothing_and_makes_no_directory(tmp_path, monkeypatch):
    monkeypatch.setattr(ca, "fetch", lambda url: pytest.fail("fetched"))
    out = tmp_path / "out"
    assert ca.main(["x", cells(tmp_path, aws="self_hosted", azure="self_hosted"), str(out)]) == 0
    assert not out.exists()


@pytest.mark.parametrize("cloud", ["azure", "gcp"])
def test_a_managed_database_in_a_cloud_with_no_fixed_bundle_is_refused_by_name_of_cloud_only(tmp_path, monkeypatch, cloud):
    monkeypatch.setattr(ca, "fetch", lambda url: pytest.fail("fetched"))
    with pytest.raises(SystemExit) as stop:
        ca.main(["x", cells(tmp_path, **{cloud: "managed"}), str(tmp_path / "out")])
    assert cloud in str(stop.value) and "no public CA bundle" in str(stop.value)


@pytest.mark.parametrize("bad", [
    "",
    "not a certificate",
    GOOD + "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n",
    GOOD + "stray text",
    "-----BEGIN CERTIFICATE-----\n!!!!\n-----END CERTIFICATE-----\n",
    "-----BEGIN CERTIFICATE-----\n" + base64.b64encode(b"plain text").decode() + "\n-----END CERTIFICATE-----\n",
])
def test_a_bundle_with_anything_but_certificates_is_refused_and_quoted_nowhere(bad):
    with pytest.raises(SystemExit) as stop:
        ca.check(bad)
    assert "PRIVATE" not in str(stop.value) and "stray" not in str(stop.value)


def test_only_https_and_a_small_download_are_accepted():
    with pytest.raises(SystemExit):
        ca.fetch("http://example.invalid/bundle.pem")
    assert all(u.startswith("https://") for u in ca.BUNDLES.values())
    assert ca.BUNDLES["aws"] == "https://truststore.pki.rds.amazonaws.com/global/global-bundle.pem"


def test_the_script_reads_nothing_from_the_environment_and_runs_no_command():
    src = (ROOT / "scripts" / "fetch_database_ca.py").read_text()
    assert "os.environ" not in src and "getenv" not in src and "subprocess" not in src
