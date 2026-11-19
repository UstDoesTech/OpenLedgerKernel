#!/usr/bin/env python3
"""Print how many resources a plan adds, changes, destroys and replaces, and nothing else.

Reads the JSON that `tofu show -json <plan>` writes. That JSON holds resource names, addresses
and, for a plan, the raw values of sensitive attributes, so this script reads it and prints
only four whole numbers. It prints nothing it read, even when the file is not what it expects.

Usage: plan_counts.py <plan.json>
"""
import json
import sys


def counts(plan: dict) -> dict:
    out = {"add": 0, "change": 0, "destroy": 0, "replace": 0}
    for change in plan.get("resource_changes", []):
        actions = tuple(change["change"]["actions"])
        if actions == ("create",):
            out["add"] += 1
        elif actions == ("update",):
            out["change"] += 1
        elif actions == ("delete",):
            out["destroy"] += 1
        elif set(actions) == {"create", "delete"} and len(actions) == 2:
            out["replace"] += 1
        elif actions in {("no-op",), ("read",)}:
            pass
        else:
            raise ValueError("unknown action")
    return out


def main(argv: list) -> int:
    if len(argv) != 2:
        print("usage: plan_counts.py <plan.json>", file=sys.stderr)
        return 2
    try:
        with open(argv[1], encoding="utf-8") as f:
            c = counts(json.load(f))
    except Exception:  # never echo the contents or the exception text
        print("::error::could not count the plan's changes")
        return 1
    print(f"Plan: {c['add']} to add, {c['change']} to change, {c['destroy']} to destroy, {c['replace']} to replace.")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
