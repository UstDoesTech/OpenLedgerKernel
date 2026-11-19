#!/usr/bin/env python3
"""Rebuild crates/ledger-core/data/iso4217.csv from SIX Group's ISO 4217 List One.

SIX is the ISO 4217 maintenance agency. List One is the current list of currencies and
funds. Standard library only, so it runs on any GitHub runner without installing anything.

    python scripts/refresh_iso4217.py                  # fetch from SIX and rewrite the file
    python scripts/refresh_iso4217.py --check          # fetch, compare, exit 1 if different
    python scripts/refresh_iso4217.py --from-file X    # use a downloaded copy of the XML

The output holds no fetch time, so running it twice on an unchanged publication changes
nothing. The first line records where the data came from and the publication date.
"""
import argparse
import re
import sys
import urllib.request
import xml.etree.ElementTree as ET
from pathlib import Path

URL = "https://www.six-group.com/dam/download/financial-information/data-center/iso-currrency/lists/list-one.xml"
OUT = Path(__file__).resolve().parent.parent / "crates" / "ledger-core" / "data" / "iso4217.csv"
HEADER = "code,numeric,minor_units"
MIN_ENTRIES = 100  # a short list means a broken download, not a real change
MUST_HAVE = {"GBP", "EUR", "USD"}


class BadList(Exception):
    pass


def fetch(url: str) -> bytes:
    req = urllib.request.Request(url, headers={"User-Agent": "open-ledger-kernel-iso4217-refresh"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return r.read()


def parse(xml: bytes) -> tuple[str, list[tuple[str, str, str]]]:
    """Return (publication date, rows of code, numeric, minor units), sorted by code."""
    root = ET.fromstring(xml)
    published = root.get("Pblshd") or "unknown"
    rows: dict[str, tuple[str, str, str]] = {}
    for entry in root.iter("CcyNtry"):
        code = (entry.findtext("Ccy") or "").strip()
        if not code:
            continue  # territories with no currency of their own
        numeric = (entry.findtext("CcyNbr") or "").strip()
        minor = (entry.findtext("CcyMnrUnts") or "").strip()
        minor = "" if minor.upper() == "N.A." else minor
        if not re.fullmatch(r"[A-Z]{3}", code):
            raise BadList(f"bad currency code {code!r}")
        if not re.fullmatch(r"[0-9]{3}", numeric):
            raise BadList(f"{code}: bad numeric code {numeric!r}")
        if minor and not re.fullmatch(r"[0-9]", minor):
            raise BadList(f"{code}: bad minor units {minor!r}")
        row = (code, numeric, minor)
        if code in rows and rows[code] != row:
            raise BadList(f"{code} appears twice with different details")
        rows[code] = row  # one currency is listed once per country that uses it
    out = sorted(rows.values())
    if len(out) < MIN_ENTRIES:
        raise BadList(f"only {len(out)} currencies; expected at least {MIN_ENTRIES}")
    missing = MUST_HAVE - {r[0] for r in out}
    if missing:
        raise BadList(f"missing {sorted(missing)}")
    numerics = [r[1] for r in out]
    if len(set(numerics)) != len(numerics):
        raise BadList("a numeric code is used by two currencies")
    return published, out


def render(published: str, rows: list[tuple[str, str, str]]) -> str:
    note = f"# source=SIX Group ISO 4217 List One ({URL}); published={published}; status=official"
    return "\n".join([note, HEADER, *(",".join(r) for r in rows)]) + "\n"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--from-file", help="read this XML file instead of fetching")
    ap.add_argument("--url", default=URL)
    ap.add_argument("--out", type=Path, default=OUT)
    ap.add_argument("--check", action="store_true", help="exit 1 if the file would change")
    a = ap.parse_args()

    xml = Path(a.from_file).read_bytes() if a.from_file else fetch(a.url)
    try:
        new = render(*parse(xml))
    except (BadList, ET.ParseError) as e:
        print(f"refusing to use this list: {e}", file=sys.stderr)
        return 2

    old = a.out.read_text() if a.out.exists() else ""
    if new == old:
        print("ISO 4217 list is unchanged")
        return 0
    if a.check:
        print("ISO 4217 list differs from the published one", file=sys.stderr)
        return 1
    a.out.parent.mkdir(parents=True, exist_ok=True)
    a.out.write_text(new)
    print(f"wrote {a.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
