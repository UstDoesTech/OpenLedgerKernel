#!/usr/bin/env python3
"""Guards for replacing one machine through the infra-apply workflow.

  replace_guard.py address <address>   Is this the address of one machine, and nothing else?
                                       (a node or member with its index, or one API machine)
  replace_guard.py plan <address> <plan.json>
                                       Does the plan replace that machine, keep every data disk
                                       and delete nothing else?

The workflow is dispatched by a person who types the address, so it is checked against a fixed
shape before it reaches tofu. The plan JSON holds resource names and raw values, and this
repository's logs are public, so this script prints a short reason and nothing it read.
"""
import json
import re
import sys

MACHINES = {"aws_instance", "azurerm_linux_virtual_machine", "google_compute_instance"}
ATTACHMENTS = {"aws_volume_attachment", "azurerm_virtual_machine_data_disk_attachment"}
DISKS = {"aws_ebs_volume", "azurerm_managed_disk", "google_compute_disk"}
# An Azure API machine has a system-assigned identity, and its role assignments take their principal
# from it, so they are recreated with the machine. This is the one thing that may be replaced
# alongside an API machine, and only in the machine's own module.
DEPENDENTS = {"azurerm_role_assignment"}

_INDEX = r'(\[([0-9]+|"[a-z0-9_.-]+")\])?'
_MODULES = rf"(module\.[a-z0-9_]+{_INDEX}\.)"
_KINDS = "|".join(sorted(MACHINES))
# Two shapes, and nothing else:
#   a database node or a control-plane member, which always has an index;
#   an API machine, which has none, because each cell has one, and which is always inside a module.
ADDRESS = re.compile(
    rf"^{_MODULES}*({_KINDS})\.(node|member)"
    r'(\[([0-9]+|"[a-z0-9_.-]+")\])$'
    rf"|^{_MODULES}+({_KINDS})\.api$"
)


def valid_address(address: str) -> bool:
    return bool(ADDRESS.fullmatch(address))


def _module_of(address: str) -> str:
    """The module path of a resource address: everything before its `<type>.<name>`."""
    return address.rsplit(".", 2)[0] if address.count(".") >= 2 else ""


def check_plan(address: str, plan: dict) -> str | None:
    """Return None if the plan is the one asked for, otherwise a reason with no names in it."""
    replaced = 0
    api = address.endswith(".api") and _module_of(address) != ""
    for change in plan.get("resource_changes", []):
        actions = set(change["change"]["actions"])
        kind = change["type"]
        if "delete" not in actions:
            continue
        if kind in DISKS:
            return "the plan would delete a data disk"
        if kind in MACHINES:
            if change["address"] != address or actions != {"create", "delete"}:
                return "the plan removes a machine other than the one asked for"
            replaced += 1
        elif api and kind in DEPENDENTS:
            # Replaced with the machine, in its own module: deleted and made again, never just deleted.
            inside = change["address"].startswith(_module_of(address) + ".")
            if actions != {"create", "delete"} or not inside:
                return "the plan deletes something other than the machine and what is made again with it"
        elif kind not in ATTACHMENTS:
            return "the plan deletes something other than the machine and its disk attachment"
    if replaced != 1:
        return "the plan does not replace exactly one machine"
    return None


def main(argv: list) -> int:
    try:
        if len(argv) == 3 and argv[1] == "address":
            if valid_address(argv[2]):
                return 0
            print("::error::the address to replace must be one machine: a node or member of an "
                  "instance or virtual machine resource, with its index, or an API machine "
                  "(module.<name>.<type>.api, with no index)")
            return 1
        if len(argv) == 4 and argv[1] == "plan":
            with open(argv[3], encoding="utf-8") as f:
                reason = check_plan(argv[2], json.load(f))
            if reason:
                print(f"::error::{reason}. Nothing was applied.")
                return 1
            return 0
    except Exception:  # never echo the contents or the exception text
        print("::error::could not check the replacement plan")
        return 1
    print("usage: replace_guard.py address <address> | plan <address> <plan.json>", file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
