"""Render the generated sections of docs/implementation-plan.md from plan/tasks.yaml.

    python plan/render.py          rewrite the generated sections in place
    python plan/render.py --check  exit 1 if the document is out of date (CI)

Only the text between <!-- generated:NAME --> and <!-- /generated:NAME --> is touched,
so the hand-written narrative around it is kept.
"""
from __future__ import annotations

import re
import sys
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parent.parent
PLAN = ROOT / "plan" / "tasks.yaml"
DOC = ROOT / "docs" / "implementation-plan.md"

STATUS_MARK = {"todo": "To do", "in_progress": "In progress", "blocked": "Blocked", "done": "Done"}


def load() -> dict:
    return yaml.safe_load(PLAN.read_text())


def _cell(text: str) -> str:
    return str(text).replace("|", "\\|").replace("\n", " ")


def decisions_table(plan: dict) -> str:
    blocks: dict[str, list[str]] = {}
    for t in plan["tasks"]:
        for d in t.get("decisions", []) or []:
            blocks.setdefault(d, []).append(t["id"])
    rows = ["| Decision | Status | Proposed default, or the decision | Blocks |", "| --- | --- | --- | --- |"]
    for d in plan["decisions"]:
        decided = d["status"] == "decided"
        text = d.get("outcome") if decided else d.get("proposed")
        # A decided decision no longer blocks anything.
        blocked = "None" if decided else ", ".join(blocks.get(d["id"], [])) or "None"
        rows.append(f"| **{d['id']}** {_cell(d['title'])} | {d['status'].capitalize()} | "
                    f"{_cell(text or 'None yet')} | {blocked} |")
    return "\n".join(rows)


def phase_section(plan: dict, phase_id: str) -> str:
    tasks = [t for t in plan["tasks"] if t["phase"] == phase_id]
    ids = {t["id"] for t in tasks}
    by_id = {t["id"]: t for t in plan["tasks"]}
    done = sum(t["status"] == "done" for t in tasks)

    lines = ["```mermaid", "flowchart LR"]
    external: set[str] = set()
    for t in tasks:
        label = f"{t['id']}<br/>{t['title']}".replace('"', "'")
        shape = ('{{"' + label + '"}}') if t["workstream"] == "GATE" else f'["{label}"]'
        lines.append(f"  {t['id'].replace('-', '_')}{shape}")
    for t in tasks:
        for dep in t.get("depends_on", []) or []:
            if dep not in ids:
                external.add(dep)
            lines.append(f"  {dep.replace('-', '_')} --> {t['id'].replace('-', '_')}")
    for dep in sorted(external):
        lines.append(f"  {dep.replace('-', '_')}([\"{dep}<br/>{by_id[dep]['phase']}\"])")
    done_ids = [t["id"].replace("-", "_") for t in tasks if t["status"] == "done"]
    if done_ids:
        lines.append("  classDef done fill:#E8F5EC,stroke:#166534,color:#14171C")
        lines.append(f"  class {','.join(done_ids)} done")
    lines.append("```")

    rows = ["| Task | Workstream | Depends on | Decisions | Size | Owner | Status |",
            "| --- | --- | --- | --- | --- | --- | --- |"]
    for t in tasks:
        rows.append(
            f"| **{t['id']}** {_cell(t['title'])} | {plan['workstreams'][t['workstream']]} | "
            f"{', '.join(t.get('depends_on') or []) or 'None'} | {', '.join(t.get('decisions') or []) or 'None'} | "
            f"{t['size']} | {t.get('owner', 'agent').capitalize()} | {STATUS_MARK[t['status']]} |")

    return (f"{done} of {len(tasks)} tasks done.\n\n" + "\n".join(lines) + "\n\n" + "\n".join(rows))


def render(text: str, plan: dict) -> str:
    sections = {"decisions": decisions_table(plan)}
    for p in plan["phases"]:
        sections[f"phase-{p['id']}"] = phase_section(plan, p["id"])

    def sub(m: re.Match) -> str:
        name = m.group(1)
        if name not in sections:
            raise KeyError(f"unknown generated section {name!r}")
        return f"<!-- generated:{name} -->\n{sections[name]}\n<!-- /generated:{name} -->"

    out, n = re.subn(r"<!-- generated:([\w-]+) -->.*?<!-- /generated:\1 -->", sub, text, flags=re.S)
    if n != len(sections):
        raise SystemExit(f"expected {len(sections)} generated sections in {DOC.name}, found {n}")
    return out


def main(argv: list[str]) -> int:
    plan = load()
    current = DOC.read_text()
    updated = render(current, plan)
    if "--check" in argv:
        if updated != current:
            print(f"{DOC.relative_to(ROOT)} is out of date: run python plan/render.py")
            return 1
        return 0
    DOC.write_text(updated)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
