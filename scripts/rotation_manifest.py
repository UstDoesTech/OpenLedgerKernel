#!/usr/bin/env python3
"""Builds the manifest the rotation reads, from the outputs of the api and authority layers.

Usage: rotation_manifest.py <api outputs.json> <authority outputs.json> <manifest.json>

The inputs are `tofu output -json` files. The manifest holds identifiers, so it is written with
mode 0600 on the runner and never printed. This prints only how many cells it found.
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


def build(api: dict, authority: dict) -> dict:
    apis = value(api, "apis")
    tenant = value(api, "tenant")
    if not isinstance(tenant, str) or not re.fullmatch(r"ten_[a-z0-9]+", tenant):
        raise SystemExit("the tenant is not a ten_ identifier")
    if not isinstance(apis, dict) or set(apis) - CLOUDS or not apis:
        raise SystemExit("the api layer lists no cell, or a cloud that is not aws, azure or gcp")
    cells = []
    for cloud in sorted(apis):
        a = apis[cloud]
        for field in ("cell", "address", "internal_port", "secret_ref"):
            if not a.get(field):
                raise SystemExit("a cell is missing a field")
        cells.append(
            {
                "name": a["cell"],
                "cloud": cloud,
                "secret": a["secret_ref"],
                "address": a["address"],
                "internal_port": int(a["internal_port"]),
            }
        )
    return {
        "authority": {
            "key_id": value(authority, "key_id"),
            "certificate_secret": value(authority, "certificate_secret"),
        },
        "tenants": [tenant],
        "cells": cells,
    }


def main(argv: list[str]) -> int:
    if len(argv) != 4:
        print(__doc__.strip().splitlines()[2], file=sys.stderr)
        return 2
    api, authority, out = argv[1:]
    with open(api) as f:
        api_outputs = json.load(f)
    with open(authority) as f:
        authority_outputs = json.load(f)
    manifest = build(api_outputs, authority_outputs)
    fd = os.open(out, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as f:
        json.dump(manifest, f)
    print(f"cells: {len(manifest['cells'])}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
