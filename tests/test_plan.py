"""The implementation plan must stay a valid, acyclic task graph that agents can follow."""
import re
import subprocess
import sys
from pathlib import Path

import pytest

yaml = pytest.importorskip("yaml")

ROOT = Path(__file__).resolve().parent.parent
PLAN = yaml.safe_load((ROOT / "plan" / "tasks.yaml").read_text())
TASKS = PLAN["tasks"]
BY_ID = {t["id"]: t for t in TASKS}
PHASES = [p["id"] for p in PLAN["phases"]]
DECISIONS = {d["id"]: d for d in PLAN["decisions"]}

REQUIRED = {"id", "title", "phase", "workstream", "size", "status", "depends_on", "deliverables", "acceptance", "refs"}
STATUSES = {"todo", "in_progress", "blocked", "done"}


def github_slug(heading: str) -> str:
    s = heading.strip().lower()
    s = re.sub(r"[^\w\- ]", "", s)
    return s.replace(" ", "-")


def anchors(md: Path) -> set[str]:
    text = re.sub(r"```.*?```", "", md.read_text(), flags=re.S)
    return {github_slug(h) for h in re.findall(r"^#{1,6}\s+(.+)$", text, flags=re.M)}


def test_ids_unique_and_well_formed():
    ids = [t["id"] for t in TASKS]
    assert len(ids) == len(set(ids)), "duplicate task ids"
    for t in TASKS:
        assert re.fullmatch(r"[A-Z]+-[0-9A-Za-z]+", t["id"]), t["id"]


def test_every_task_has_required_fields():
    for t in TASKS:
        missing = REQUIRED - set(t)
        assert not missing, f"{t['id']} missing {missing}"
        assert t["acceptance"], f"{t['id']} has no acceptance criteria"
        assert t["deliverables"], f"{t['id']} has no deliverables"
        assert t["status"] in STATUSES, t["id"]
        assert t["size"] in {"S", "M", "L"}, t["id"]
        assert t["phase"] in PHASES, t["id"]
        assert t["workstream"] in PLAN["workstreams"], t["id"]
        assert t.get("owner", "agent") in {"agent", "human"}, t["id"]


def test_dependencies_exist_and_point_backwards_in_phase_order():
    for t in TASKS:
        for d in t["depends_on"]:
            assert d in BY_ID, f"{t['id']} depends on unknown {d}"
            assert PHASES.index(BY_ID[d]["phase"]) <= PHASES.index(t["phase"]), f"{t['id']} depends on later phase {d}"


def test_graph_is_acyclic():
    state: dict[str, int] = {}

    def visit(n: str, path: list[str]) -> None:
        if state.get(n) == 1:
            raise AssertionError("cycle: " + " -> ".join(path + [n]))
        if state.get(n) == 2:
            return
        state[n] = 1
        for d in BY_ID[n]["depends_on"]:
            visit(d, path + [n])
        state[n] = 2

    for t in TASKS:
        visit(t["id"], [])


def test_every_later_phase_task_waits_for_the_previous_gate():
    gates = {p["id"]: p["gate"] for p in PLAN["phases"]}

    def ancestors(n: str, seen: set[str]) -> set[str]:
        for d in BY_ID[n]["depends_on"]:
            if d not in seen:
                seen.add(d)
                ancestors(d, seen)
        return seen

    for i, phase in enumerate(PHASES[1:], start=1):
        gate = gates[PHASES[i - 1]]
        for t in TASKS:
            if t["phase"] == phase:
                assert gate in ancestors(t["id"], set()), f"{t['id']} does not wait for {gate}"


def test_gates_are_human_and_close_their_phase():
    for p in PLAN["phases"]:
        if p["gate"]:
            g = BY_ID[p["gate"]]
            assert g.get("owner") == "human", p["gate"]
            assert g["phase"] == p["id"], p["gate"]


def test_decisions_are_known():
    for d in DECISIONS.values():
        assert d["status"] in {"open", "decided"}, d["id"]
    for t in TASKS:
        for d in t.get("decisions", []) or []:
            assert d in DECISIONS, f"{t['id']} lists unknown decision {d}"


def test_done_tasks_have_done_dependencies():
    for t in TASKS:
        if t["status"] == "done":
            for d in t["depends_on"]:
                assert BY_ID[d]["status"] == "done", f"{t['id']} is done but {d} is not"


def test_refs_resolve():
    for t in TASKS:
        for ref in t["refs"]:
            path, _, anchor = ref.partition("#")
            target = ROOT / path
            assert target.exists(), f"{t['id']} refers to missing {path}"
            if anchor:
                assert anchor in anchors(target), f"{t['id']} refers to missing heading #{anchor} in {path}"


def test_rendered_plan_is_up_to_date():
    r = subprocess.run([sys.executable, str(ROOT / "plan" / "render.py"), "--check"], capture_output=True, text=True)
    assert r.returncode == 0, r.stdout + r.stderr
