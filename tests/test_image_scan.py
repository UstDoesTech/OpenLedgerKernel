"""I-01i: the image scanner, tried on small synthetic images."""
import importlib.util
import io
import json
import sys
import tarfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("scan_image_layers", ROOT / "scripts" / "scan_image_layers.py")
scanner = importlib.util.module_from_spec(spec)
sys.modules["scan_image_layers"] = scanner
spec.loader.exec_module(scanner)

KEY = b"-----BEGIN PRIVATE KEY-----\nMIIE\n-----END PRIVATE KEY-----\n"


def layer(files: dict[str, bytes]) -> bytes:
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w") as tar:
        for name, data in files.items():
            info = tarfile.TarInfo(name)
            info.size = len(data)
            tar.addfile(info, io.BytesIO(data))
    return buffer.getvalue()


def image(tmp_path: Path, layers: list[dict[str, bytes]], user="postgres", env=None) -> Path:
    path = tmp_path / "image.tar"
    config = {"config": {"User": user, "Env": env or ["PATH=/usr/bin"]}}
    with tarfile.open(path, "w") as tar:
        def add(name, data):
            info = tarfile.TarInfo(name)
            info.size = len(data)
            tar.addfile(info, io.BytesIO(data))
        names = []
        for i, files in enumerate(layers):
            names.append(f"l{i}/layer.tar")
            add(names[-1], layer(files))
        add("config.json", json.dumps(config).encode())
        add("manifest.json", json.dumps([{"Config": "config.json", "Layers": names}]).encode())
    return path


def test_a_clean_image_passes(tmp_path):
    assert scanner.scan(image(tmp_path, [{"usr/bin/app": b"\x7fELF", "etc/ssl/ca.pem": b"-----BEGIN CERTIFICATE-----\nx\n"}])) == []


def test_a_private_key_is_found(tmp_path):
    problems = scanner.scan(image(tmp_path, [{"etc/app/server.key": KEY}]))
    assert problems == ["/etc/app/server.key: private key"]


def test_a_key_deleted_by_a_later_layer_does_not_count(tmp_path):
    layers = [{"etc/ssl/private/snakeoil.key": KEY}, {"etc/ssl/private/.wh.snakeoil.key": b""}]
    assert scanner.scan(image(tmp_path, layers)) == []


def test_a_key_in_a_directory_cleared_by_a_later_layer_does_not_count(tmp_path):
    layers = [{"opt/keys/a.key": KEY}, {"opt/keys/.wh..wh..opq": b""}]
    assert scanner.scan(image(tmp_path, layers)) == []


def test_a_key_replaced_by_a_later_layer_does_not_count(tmp_path):
    layers = [{"etc/a.key": KEY}, {"etc/a.key": b"empty"}]
    assert scanner.scan(image(tmp_path, layers)) == []


def test_cloud_credentials_are_found(tmp_path):
    problems = scanner.scan(image(tmp_path, [{"root/.aws": b"id AKIAABCDEFGHIJKLMNOP"}, {"etc/sa.json": b'{"type": "service_account"}'}]))
    assert len(problems) == 2


def test_a_secret_in_the_environment_is_found(tmp_path):
    problems = scanner.scan(image(tmp_path, [{"a": b"x"}], env=["POSTGRES_PASSWORD=hunter2"]))
    assert problems == ["environment variable POSTGRES_PASSWORD has a value baked into the image"]


def test_an_empty_secret_variable_is_not_a_secret(tmp_path):
    assert scanner.scan(image(tmp_path, [{"a": b"x"}], env=["POSTGRES_PASSWORD="])) == []


def test_running_as_root_fails(tmp_path):
    for user in ("", "root", "0", "0:0"):
        assert scanner.scan(image(tmp_path, [{"a": b"x"}], user=user)) == ["the image runs as root"], user
    assert scanner.scan(image(tmp_path, [{"a": b"x"}], user="postgres")) == []
