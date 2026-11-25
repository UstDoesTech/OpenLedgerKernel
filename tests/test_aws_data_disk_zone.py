"""A data volume must never take its zone from the machine it is attached to.

Replacing a machine makes its zone unknown until the new one exists, and a volume whose zone is
unknown is planned for replacement. The plan then deletes the data disk, which the replace guard
refuses, and which would lose the data if it were applied. The zone comes from the subnet instead.
"""
import re
from pathlib import Path

import pytest

MODULES = Path(__file__).resolve().parent.parent / "infra" / "modules"
FILES = {
    "node": MODULES / "postgres" / "aws-self-hosted" / "main.tf",
    "member": MODULES / "control-plane" / "aws-members" / "main.tf",
}


def volume(path):
    text = path.read_text()
    return text.split('resource "aws_ebs_volume" "data"')[1].split("\nresource ")[0]


@pytest.mark.parametrize("kind", FILES)
def test_the_volume_takes_its_zone_from_the_subnet(kind):
    zone = re.search(r"availability_zone\s*=\s*(.+)", volume(FILES[kind])).group(1)
    assert zone.startswith(f"data.aws_subnet.{kind}["), zone
    assert zone.endswith(".availability_zone"), zone


@pytest.mark.parametrize("kind", FILES)
def test_nothing_about_the_volume_refers_to_the_machine(kind):
    assert "aws_instance" not in volume(FILES[kind])


@pytest.mark.parametrize("kind", FILES)
def test_the_subnet_is_read_by_its_id(kind):
    text = FILES[kind].read_text()
    block = text.split(f'data "aws_subnet" "{kind}"')[1].split("\n}")[0]
    assert "var.subnet_ids[" in block
