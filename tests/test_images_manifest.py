"""I-01p: the image manifest names each image exactly, and the layers read it."""
import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MANIFEST = ROOT / "infra" / "images.json"
CONTROL_PLANE = ROOT / "infra" / "envs" / "europe" / "control-plane"


def manifest():
    return json.loads(MANIFEST.read_text())


def test_the_etcd_image_is_pinned_by_a_digest_from_an_https_free_registry_name():
    etcd = manifest()["etcd"]
    assert re.fullmatch(r"sha256:[0-9a-f]{64}", etcd["digest"]), "pin the real digest, not a placeholder"
    assert re.fullmatch(r"[a-z0-9.\-]+(:[0-9]+)?(/[a-z0-9._\-]+)+", etcd["source"]), etcd["source"]
    assert "://" not in etcd["source"]
    assert re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+", etcd["tag"]), "a version tag, never latest"


def test_one_flatcar_release_is_pinned():
    flatcar = manifest()["flatcar"]
    assert flatcar["channel"] in {"stable", "lts"}
    assert re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", flatcar["version"]), "pin the real release, not a placeholder"


def test_the_manifest_holds_only_what_it_should():
    assert set(manifest()) == {"api", "etcd", "flatcar", "postgres"}
    assert set(manifest()["postgres"]) == {"source", "digest"}
    assert set(manifest()["etcd"]) == {"source", "tag", "digest"}
    assert set(manifest()["flatcar"]) == {"channel", "version"}


def test_the_control_plane_layer_reads_the_manifest_and_takes_no_image_secrets():
    text = "\n".join(p.read_text() for p in CONTROL_PLANE.glob("*.tf"))
    assert 'file("${path.module}/../../../images.json")' in text
    for gone in ("etcd_image_digest", "aws_flatcar_ami_id", "gcp_flatcar_image"):
        assert f'variable "{gone}"' not in text, gone
    # The public images are looked up by the pinned release, never by "latest" or a family.
    assert "local.flatcar.version" in text
    assert "family" not in text


def test_the_postgres_image_is_our_own_package_pinned_by_digest_with_no_tag():
    postgres = manifest()["postgres"]
    assert re.fullmatch(r"sha256:[0-9a-f]{64}", postgres["digest"])
    # Built by publish-postgres-image into this repository's own package; the repository name
    # is lower-cased as GitHub does it. No tag selects it.
    assert postgres["source"] == "ghcr.io/ustdoestech/openledgerkernel/postgres"
    assert "tag" not in postgres



def test_the_api_image_is_our_own_package_pinned_by_digest_with_no_tag():
    api = manifest()["api"]
    assert re.fullmatch(r"sha256:[0-9a-f]{64}", api["digest"])
    # Built by publish-api-image into this repository's own package. No tag selects it.
    assert api["source"] == "ghcr.io/ustdoestech/openledgerkernel/api"
    assert "tag" not in api
