#!/usr/bin/env python3
"""Fetches the public CA bundle for each managed database, for the rotation's manifest (K-24f).

Usage: fetch_database_ca.py <cells outputs.json> <output directory>

For each cloud whose cell is managed, it downloads that cloud's published bundle, checks that it holds
certificates and nothing else, and writes `<cloud>.pem` in the output directory. A self-hosted cell
gets nothing. The bundle is public, not a secret. This prints only how many bundles it wrote.
"""
import base64
import binascii
import json
import os
import re
import sys
import urllib.request

# The cloud's own published address for the bundle that signs its managed database's certificate.
# Only Amazon is here so far. Azure rotates its roots and intermediates on its own schedule, and
# Cloud SQL signs with a certificate authority of each instance, so neither has one fixed address.
# Amazon's global bundle holds over a hundred certificates, which is more than a secret can hold
# (AWS Secrets Manager: 65,536 bytes), so the cell's own region's bundle is used instead.
REGIONAL = {
    "aws": "https://truststore.pki.rds.amazonaws.com/{region}/{region}-bundle.pem",
}
BUNDLES = REGIONAL
REGION = re.compile(r"\.([a-z]{2}(?:-[a-z]+)+-[0-9]+)\.rds\.amazonaws\.com\.?$")
# The bundle goes into the API secret with the other files. Secrets Manager holds 65,536 bytes in all and
# Key Vault holds 25 KB, so a bundle larger than this is refused before it is written anywhere.
MAX_BUNDLE = 20_000
LIMIT = 1_000_000
BLOCK = re.compile(r"-----BEGIN CERTIFICATE-----\s+([A-Za-z0-9+/=\s]+?)\s*-----END CERTIFICATE-----")


def fetch(url: str) -> str:
    if not url.startswith("https://"):
        raise SystemExit("a CA bundle comes over https only")
    with urllib.request.urlopen(url, timeout=30) as reply:  # noqa: S310 (https only, fixed addresses)
        data = reply.read(LIMIT + 1)
    if len(data) > LIMIT:
        raise SystemExit("a CA bundle is larger than a CA bundle should be")
    return data.decode("ascii", "strict")


def check(pem: str) -> str:
    """The bundle if it is certificates and whitespace only, else a refusal that quotes nothing."""
    rest = BLOCK.sub("", pem)
    blocks = BLOCK.findall(pem)
    if not blocks or rest.strip():
        raise SystemExit("a CA bundle holds something that is not a certificate")
    for body in blocks:
        try:
            der = base64.b64decode("".join(body.split()), validate=True)
        except binascii.Error:
            raise SystemExit("a CA bundle holds a certificate that is not base64") from None
        if not der.startswith(b"\x30"):
            raise SystemExit("a CA bundle holds a certificate that is not DER")
    pem = pem if pem.endswith("\n") else pem + "\n"
    if len(pem.encode("ascii")) > MAX_BUNDLE:
        raise SystemExit("a CA bundle is too large to be kept in the API secret")
    return pem


def url_for(cloud: str, cell: dict) -> str:
    """The address of the bundle for this cell. AWS gives its region in the database's host name."""
    template = REGIONAL[cloud]
    if "{region}" not in template:
        return template
    found = REGION.search(str(cell.get("host") or ""))
    if not found:
        raise SystemExit(f"a managed database in {cloud} has no host this run can read a region from")
    return template.format(region=found.group(1))


def main(argv: list[str]) -> int:
    if len(argv) != 3:
        print(__doc__.strip().splitlines()[2], file=sys.stderr)
        return 2
    with open(argv[1]) as f:
        cells = json.load(f).get("cells", {}).get("value")
    if not isinstance(cells, dict):
        raise SystemExit("the cells layer's outputs have no cells")
    out = argv[2]
    written = 0
    for cloud in sorted(cells):
        if cells[cloud].get("kind") != "managed":
            continue
        if cloud not in REGIONAL:
            raise SystemExit(f"a managed database in {cloud} has no public CA bundle this run can fetch yet")
        pem = check(fetch(url_for(cloud, cells[cloud])))
        os.makedirs(out, mode=0o700, exist_ok=True)
        with open(os.path.join(out, f"{cloud}.pem"), "w") as f:
            f.write(pem)
        written += 1
    print(f"database CA bundles: {written}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
