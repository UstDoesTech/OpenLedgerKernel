"""The API spec must be valid OpenAPI 3.1 and keep the design's non-negotiables."""
from pathlib import Path

import pytest

yaml = pytest.importorskip("yaml")
validator = pytest.importorskip("openapi_spec_validator")

SPEC = yaml.safe_load((Path(__file__).resolve().parent.parent / "api" / "openapi.yaml").read_text())


def test_spec_is_valid_openapi():
    validator.validate(SPEC)


def test_every_write_requires_an_idempotency_key():
    for path, ops in SPEC["paths"].items():
        for method, op in ops.items():
            if method != "post":
                continue
            refs = [p.get("$ref", "") for p in op.get("parameters", [])]
            assert "#/components/parameters/IdempotencyKey" in refs, f"POST {path}"


def test_no_resource_is_written_directly():
    for path, ops in SPEC["paths"].items():
        assert not {"put", "patch", "delete"} & set(ops), f"{path} has a direct write"


def test_money_is_integer_minor_units():
    money = SPEC["components"]["schemas"]["Money"]
    assert money["properties"]["minor"]["type"] == "integer"
    assert money["additionalProperties"] is False


def test_tenant_never_appears_in_paths_or_params():
    for path, ops in SPEC["paths"].items():
        assert "tenant" not in path
        for op in ops.values():
            for p in op.get("parameters", []):
                assert "tenant" not in p.get("name", "")
