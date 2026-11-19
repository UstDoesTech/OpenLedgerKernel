"""A module call must not use depends_on.

A depends_on on a module makes every data source inside it wait for the thing it names. When
that thing has a change pending (a new image digest, say), the data sources are not read at plan
time, and anything built from them becomes "known after apply". The Google zones lookup did this:
the zone of each data disk and machine became unknown, which forces replacement, so a plan for a
digest change wanted to destroy the database disks. The inputs check runs at plan time anyway
(its values are known then), so the ordering bought nothing.
"""

import re
from pathlib import Path

INFRA = Path(__file__).resolve().parent.parent / "infra"


def module_blocks(text):
    for m in re.finditer(r'(?m)^module "[^"]+" \{', text):
        depth, i = 0, m.end() - 1
        for j in range(i, len(text)):
            depth += text[j] == "{"
            depth -= text[j] == "}"
            if depth == 0:
                yield m.group(0), text[i : j + 1]
                break


def test_no_module_call_has_depends_on():
    bad = []
    for tf in INFRA.rglob("*.tf"):
        for head, body in module_blocks(tf.read_text()):
            if re.search(r"(?m)^  depends_on\s*=", body):
                bad.append(f"{tf.relative_to(INFRA)}: {head}")
    assert not bad, bad


def test_the_inputs_checks_still_exist_as_preconditions():
    for rel in ("modules/postgres/main.tf", "modules/control-plane/main.tf"):
        text = (INFRA / rel).read_text()
        assert 'resource "terraform_data" "inputs"' in text and "precondition" in text, rel
