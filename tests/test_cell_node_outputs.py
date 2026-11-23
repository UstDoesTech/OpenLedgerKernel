"""K-20a: a self-hosted cell's nodes and certificate secrets are in the cells layer's outputs."""
import re
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent
POSTGRES = ROOT / "infra" / "modules" / "postgres"
CELLS = ROOT / "infra" / "envs" / "europe" / "cells"

# Where each module makes the secret that holds a node's certificates.
TLS_SECRET = {
    "aws": ("aws_secretsmanager_secret", "arn"),
    "azure": ("azurerm_key_vault_secret", "versionless_id"),
    "gcp": ("google_secret_manager_secret", "id"),
}


def output_block(src: str, name: str) -> str:
    m = re.search(rf'output "{name}" \{{(.*?)\n\}}', src, re.S)
    assert m, f"no output {name}"
    return m.group(1)


@pytest.mark.parametrize("cloud", sorted(TLS_SECRET))
def test_each_module_outputs_its_three_nodes_with_the_secret_that_holds_their_certificates(cloud):
    module = POSTGRES / f"{cloud}-self-hosted"
    block = output_block((module / "outputs.tf").read_text(), "nodes")
    assert "for i in range(3)" in block
    for field in ("name", "role", "address", "secret_ref"):
        assert re.search(rf"^\s*{field}\s*=", block, re.M), field
    assert "local.names[i]" in block and "local.addresses[i]" in block
    assert 'i == 0 ? "primary" : "standby"' in block
    resource, attribute = TLS_SECRET[cloud]
    assert f"{resource}.tls[i].{attribute}" in block
    # The secret it names is the one the node boots from, so a node and its certificates match.
    main = (module / "main.tf").read_text()
    assert f"{resource}.tls[count.index]" in main


@pytest.mark.parametrize("cloud", sorted(TLS_SECRET))
def test_the_nodes_output_holds_no_certificate_key_or_password(cloud):
    block = output_block((POSTGRES / f"{cloud}-self-hosted" / "outputs.tf").read_text(), "nodes")
    code = "\n".join(l for l in block.splitlines() if not l.lstrip().startswith("description"))
    assert not re.search(r"secret_string|secret_data|secret_version|private_key|password|sensitive", code, re.I)


def test_the_postgres_module_gathers_the_nodes_of_a_self_hosted_cell_and_none_for_others():
    block = output_block((POSTGRES / "outputs.tf").read_text(), "nodes")
    for cloud in TLS_SECRET:
        assert f"module.{cloud}_self_hosted[*].nodes" in block
    assert "flatten(" in block
    for other in ("aws_managed", "azure_managed", "gcp_managed", "lakebase"):
        assert other not in block


def test_the_cells_layer_gives_each_cell_its_nodes():
    src = (CELLS / "outputs.tf").read_text()
    cells = output_block(src, "cells")
    for cloud in TLS_SECRET:
        assert f"nodes      = module.cell_{cloud}.nodes" in cells
    assert not re.search(r"sensitive|secret_string|password", cells, re.I)


def test_the_document_says_where_the_nodes_certificates_go():
    doc = (ROOT / "docs" / "self-hosted-postgres.md").read_text()
    assert "`nodes`" in doc and "cells" in doc
