#!/usr/bin/env python3
"""Builds the manifest the rotation reads, from the outputs of the api and authority layers.

Usage: rotation_manifest.py <api outputs.json> <authority outputs.json> <manifest.json> [<cells outputs.json> [proving]]

The inputs are `tofu output -json` files. Giving the cells layer's outputs adds each cell's own
database: where it is, where its administrator's credential is held and, for a self-hosted cell, its
machines and the secrets their certificates go in (K-20b). The last word, `proving`, marks the manifest as a
proving environment's (D-19), so the run may also fill a missing db.conn and tokens.json. The manifest holds
identifiers, so it is written with mode 0600 on the runner and never printed. This prints only how many
cells it found.
"""
import json
import os
import re
import sys

CLOUDS = {"aws", "azure", "gcp"}


def value(outputs: dict, name: str):
    try:
        return outputs[name]["value"]
    except (KeyError, TypeError):
        raise SystemExit(f"an expected output is missing: {name}")


KINDS = {"self_hosted", "managed"}
PLAIN = re.compile(r"[A-Za-z0-9._:-]+")


def databases(cells_outputs: dict) -> dict:
    """Each cloud's own database: the name its certificate carries, the port and the secret that
    holds the administrator's credential. Never the credential."""
    cells = value(cells_outputs, "cells")
    if not isinstance(cells, dict) or set(cells) - CLOUDS:
        raise SystemExit("the cells layer lists a cloud that is not aws, azure or gcp")
    out = {}
    for cloud, c in cells.items():
        host, port = c.get("host"), c.get("port")
        if not (isinstance(host, str) and PLAIN.fullmatch(host)):
            raise SystemExit("a cell's database host is not a plain name")
        if not isinstance(port, int) or not 0 < port < 65536:
            raise SystemExit("a cell's database port is not a port")
        if c.get("kind") not in KINDS or not c.get("secret_ref"):
            raise SystemExit("a cell's database has no kind or credential secret")
        out[cloud] = {
            "host": host,
            "port": port,
            "kind": c["kind"],
            "credential_secret": c["secret_ref"],
            "nodes": nodes(c),
        }
    return out


ROLES = {"primary", "standby"}


def nodes(cell: dict) -> list:
    """A self-hosted cell's machines, each with the secret its certificates go in. Never a certificate."""
    listed = cell.get("nodes") or []
    if listed and cell["kind"] != "self_hosted":
        raise SystemExit("a cell that someone else runs lists machines")
    out = []
    for n in listed:
        name, role, address, secret = (n.get(k) for k in ("name", "role", "address", "secret_ref"))
        if not (isinstance(name, str) and PLAIN.fullmatch(name)) or role not in ROLES:
            raise SystemExit("a machine has no plain name or no role")
        if not (isinstance(address, str) and PLAIN.fullmatch(address)) or not secret:
            raise SystemExit("a machine has no address or no certificate secret")
        out.append({"name": name, "role": role, "address": address, "secret": secret})
    return out


def build(api: dict, authority: dict, cells_outputs: dict | None = None, proving: bool = False) -> dict:
    apis = value(api, "apis")
    tenant = value(api, "tenant")
    if not isinstance(tenant, str) or not re.fullmatch(r"ten_[a-z0-9]+", tenant):
        raise SystemExit("the tenant is not a ten_ identifier")
    if not isinstance(apis, dict) or set(apis) - CLOUDS or not apis:
        raise SystemExit("the api layer lists no cell, or a cloud that is not aws, azure or gcp")
    dbs = databases(cells_outputs) if cells_outputs is not None else {}
    cells = []
    for cloud in sorted(apis):
        a = apis[cloud]
        for field in ("cell", "address", "internal_port", "secret_ref"):
            if not a.get(field):
                raise SystemExit("a cell is missing a field")
        entry = {
            "name": a["cell"],
            "cloud": cloud,
            "secret": a["secret_ref"],
            "address": a["address"],
            "internal_port": int(a["internal_port"]),
        }
        if cells_outputs is not None:
            if cloud not in dbs:
                raise SystemExit("the cells layer has no database for a cell the api layer lists")
            entry["database"] = dbs[cloud]
        cells.append(entry)
    manifest = {
        "authority": {
            "key_id": value(authority, "key_id"),
            "certificate_secret": value(authority, "certificate_secret"),
        },
        "tenants": [tenant],
        "cells": cells,
    }
    if proving:
        if cells_outputs is None:
            raise SystemExit("a proving environment needs the cells layer's outputs")
        manifest["proving"] = True
    return manifest


def main(argv: list[str]) -> int:
    if len(argv) not in (4, 5, 6) or (len(argv) == 6 and argv[5] != "proving"):
        print(__doc__.strip().splitlines()[2], file=sys.stderr)
        return 2
    api, authority, out = argv[1:4]
    with open(api) as f:
        api_outputs = json.load(f)
    with open(authority) as f:
        authority_outputs = json.load(f)
    cells_outputs = None
    if len(argv) >= 5:
        with open(argv[4]) as f:
            cells_outputs = json.load(f)
    manifest = build(api_outputs, authority_outputs, cells_outputs, proving=len(argv) == 6)
    fd = os.open(out, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as f:
        json.dump(manifest, f)
    print(f"cells: {len(manifest['cells'])}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
