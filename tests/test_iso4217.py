"""The ISO 4217 refresh script, run offline against XML shaped like SIX Group's List One."""
import importlib.util
import subprocess
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts" / "refresh_iso4217.py"
SEED = ROOT / "crates" / "ledger-core" / "data" / "iso4217.csv"

spec = importlib.util.spec_from_file_location("refresh_iso4217", SCRIPT)
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)


def seed_rows():
    lines = [l for l in SEED.read_text().splitlines() if l and not l.startswith("#")][1:]
    return [tuple(l.split(",")) for l in lines]


def list_one(rows, published="2026-01-01", extra=""):
    """XML in the shape of List One: one entry per country, several countries per currency."""
    body = ['<CcyNtry><CtryNm>ANTARCTICA</CtryNm><CcyNm>No universal currency</CcyNm></CcyNtry>']
    for code, num, minor in rows:
        for country in ("ONE", "TWO") if code == "EUR" else ("ONE",):
            body.append(
                f"<CcyNtry><CtryNm>{country}</CtryNm><CcyNm>name</CcyNm><Ccy>{code}</Ccy>"
                f"<CcyNbr>{num}</CcyNbr><CcyMnrUnts>{minor or 'N.A.'}</CcyMnrUnts></CcyNtry>"
            )
    return f'<?xml version="1.0"?><ISO_4217 Pblshd="{published}"><CcyTbl>{"".join(body)}{extra}</CcyTbl></ISO_4217>'.encode()


def run(*args):
    return subprocess.run([sys.executable, str(SCRIPT), *args], capture_output=True, text=True)


def test_parse_collapses_countries_and_skips_territories_without_a_currency():
    published, rows = mod.parse(list_one(seed_rows(), "2026-03-01"))
    assert published == "2026-03-01"
    assert rows == sorted(seed_rows())  # EUR appears for two countries but once in the output


def test_rewrites_the_file_once_then_reports_no_change(tmp_path):
    xml = tmp_path / "l1.xml"
    xml.write_bytes(list_one(seed_rows(), "2026-03-01"))
    out = tmp_path / "iso.csv"
    assert run("--from-file", str(xml), "--out", str(out)).returncode == 0
    first = out.read_text()
    assert first.splitlines()[0].endswith("published=2026-03-01; status=official")
    r = run("--from-file", str(xml), "--out", str(out))
    assert "unchanged" in r.stdout and out.read_text() == first
    assert run("--from-file", str(xml), "--out", str(out), "--check").returncode == 0


def test_check_fails_when_the_published_list_differs(tmp_path):
    out = tmp_path / "iso.csv"
    xml = tmp_path / "l1.xml"
    xml.write_bytes(list_one(seed_rows()))
    run("--from-file", str(xml), "--out", str(out))
    changed = [r for r in seed_rows() if r[0] != "JPY"] + [("ZZZ", "998", "2")]
    xml.write_bytes(list_one(changed))
    assert run("--from-file", str(xml), "--out", str(out), "--check").returncode == 1
    assert "ZZZ" not in out.read_text()  # --check never writes


@pytest.mark.parametrize(
    "mutate",
    [
        lambda rows: rows[:20],  # truncated download
        lambda rows: [r for r in rows if r[0] != "GBP"],  # missing a currency we rely on
        lambda rows: rows + [("gbp", "999", "2")],  # bad code
        lambda rows: rows + [("QQQ", "82", "2")],  # bad number
        lambda rows: rows + [("QQQ", "826", "2")],  # number clash with GBP
    ],
)
def test_a_broken_list_is_refused_and_leaves_the_file_alone(tmp_path, mutate):
    xml = tmp_path / "l1.xml"
    xml.write_bytes(list_one(mutate(seed_rows())))
    out = tmp_path / "iso.csv"
    out.write_text("keep me\n")
    r = run("--from-file", str(xml), "--out", str(out))
    assert r.returncode == 2 and out.read_text() == "keep me\n"


def test_not_xml_is_refused(tmp_path):
    xml = tmp_path / "l1.xml"
    xml.write_text("<html>blocked</html")
    assert run("--from-file", str(xml), "--out", str(tmp_path / "o.csv")).returncode == 2


def test_the_embedded_file_says_where_it_came_from():
    note = SEED.read_text().splitlines()[0]
    assert note.startswith("# source=") and ("status=official" in note or "status=provisional" in note)
