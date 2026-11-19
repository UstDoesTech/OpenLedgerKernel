#!/usr/bin/env python3
"""Fails when a container image holds a secret.

Reads the tarball that `docker save` writes, replays its layers in order (a file deleted by a
later layer does not count), and looks in what is left for private keys, cloud access key ids
and secret-looking environment variables. It also fails an image that runs as root.

    docker save ledger-postgres:ci -o image.tar
    python scripts/scan_image_layers.py image.tar
"""
from __future__ import annotations

import json
import posixpath
import re
import sys
import tarfile
from pathlib import Path

MAX_FILE = 2 * 1024 * 1024
RULES = {
    "private key": re.compile(rb"-----BEGIN (?:[A-Z0-9]+ )?PRIVATE KEY-----"),
    "AWS access key id": re.compile(rb"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b"),
    "Google service account key": re.compile(rb'"type"\s*:\s*"service_account"'),
    "Azure storage key": re.compile(rb"AccountKey=[A-Za-z0-9+/=]{40,}"),
}
SECRET_ENV = re.compile(r"(PASSWORD|PASSWD|SECRET|TOKEN|PRIVATE_KEY|ACCESS_KEY|API_KEY)", re.I)


def layer_paths(archive: tarfile.TarFile) -> tuple[list[str], str | None]:
    manifest = json.load(archive.extractfile("manifest.json"))[0]
    return manifest["Layers"], manifest.get("Config")


def scan_layers(archive: tarfile.TarFile, layers: list[str]) -> dict[str, str]:
    """The findings in the merged file system: path -> rule."""
    found: dict[str, str] = {}
    for name in layers:
        member = archive.extractfile(name)
        with tarfile.open(fileobj=member, mode="r|*") as layer:
            for entry in layer:
                path = "/" + entry.name.lstrip("./").rstrip("/")
                base = posixpath.basename(path)
                parent = posixpath.dirname(path)
                if base == ".wh..wh..opq":
                    for key in [k for k in found if k.startswith(parent.rstrip("/") + "/")]:
                        del found[key]
                    continue
                if base.startswith(".wh."):
                    gone = posixpath.join(parent, base[4:])
                    for key in [k for k in found if k == gone or k.startswith(gone + "/")]:
                        del found[key]
                    continue
                found.pop(path, None)  # replaced by this layer
                if not entry.isreg() or entry.size > MAX_FILE:
                    continue
                data = layer.extractfile(entry).read()
                for rule, pattern in RULES.items():
                    if pattern.search(data):
                        found[path] = rule
                        break
    return found


def scan_config(config: dict) -> list[str]:
    problems = []
    settings = config.get("config", {})
    for item in settings.get("Env") or []:
        name, _, value = item.partition("=")
        if SECRET_ENV.search(name) and value:
            problems.append(f"environment variable {name} has a value baked into the image")
    user = (settings.get("User") or "").split(":")[0]
    if user in ("", "root", "0"):
        problems.append("the image runs as root")
    return problems


def scan(path: Path) -> list[str]:
    with tarfile.open(path) as archive:
        layers, config_name = layer_paths(archive)
        problems = [f"{p}: {rule}" for p, rule in sorted(scan_layers(archive, layers).items())]
        if config_name:
            problems += scan_config(json.load(archive.extractfile(config_name)))
    return problems


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print(__doc__)
        return 2
    problems = scan(Path(argv[1]))
    for problem in problems:
        print(f"FOUND {problem}")
    if problems:
        return 1
    print("Image scan: no secrets in the final file system or the image settings, and it does not run as root")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
