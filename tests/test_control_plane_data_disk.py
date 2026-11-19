"""The control plane's data disks are their own resources on every cloud, so replacing a member
to roll a new image detaches the disk, replaces the machine and attaches the same disk again."""
import re
from pathlib import Path

import pytest

MEMBERS = Path(__file__).resolve().parent.parent / "infra" / "modules" / "control-plane"


def body(form: str) -> str:
    return "\n".join(
        "\n".join(l for l in p.read_text().splitlines() if not l.lstrip().startswith("#"))
        for p in sorted((MEMBERS / form).glob("*.tf"))
    )


DISKS = {
    "aws-members": ("aws_ebs_volume", "aws_volume_attachment"),
    "azure-members": ("azurerm_managed_disk", "azurerm_virtual_machine_data_disk_attachment"),
    "gcp-members": ("google_compute_disk", None),
}


@pytest.mark.parametrize("form", DISKS)
def test_the_data_disk_is_its_own_resource(form):
    disk, attachment = DISKS[form]
    s = body(form)
    assert f'resource "{disk}" "data"' in s
    if attachment:
        assert f'resource "{attachment}" "data"' in s


def test_aws_members_have_no_inline_disk_and_the_volume_is_encrypted_with_the_tenant_key():
    s = body("aws-members")
    assert "ebs_block_device" not in s
    volume = s.split('resource "aws_ebs_volume" "data"')[1].split("\nresource ")[0]
    assert re.search(r"encrypted\s+= true", volume)
    assert re.search(r"kms_key_id\s+= var\.kms_key_arn", volume)


def test_aws_attachment_stops_the_machine_before_it_detaches():
    s = body("aws-members")
    attach = s.split('resource "aws_volume_attachment" "data"')[1]
    assert re.search(r"stop_instance_before_detaching\s+= true", attach)
