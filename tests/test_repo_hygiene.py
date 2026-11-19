"""Files that keep a public repository safe to run in the open."""

import re
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parent.parent


def test_security_policy_gives_a_private_route_and_says_what_not_to_post():
    s = (ROOT / "SECURITY.md").read_text()
    assert "Report a vulnerability" in s and "privately" in s
    for word in ("identifiers", "plan", "keys", "personal data"):
        assert word in s


def test_dependabot_covers_actions_rust_python_and_the_node_image():
    d = yaml.safe_load((ROOT / ".github" / "dependabot.yml").read_text())
    kinds = {u["package-ecosystem"] for u in d["updates"]}
    assert {"github-actions", "cargo", "pip", "docker"} <= kinds


def test_issue_templates_warn_against_identifiers_and_blank_issues_are_off():
    cfg = yaml.safe_load((ROOT / ".github" / "ISSUE_TEMPLATE" / "config.yml").read_text())
    assert cfg["blank_issues_enabled"] is False
    for name in ("bug.yml", "feature.yml"):
        text = (ROOT / ".github" / "ISSUE_TEMPLATE" / name).read_text()
        assert "No identifiers" in text or "Do not paste account" in text, name
    assert "identifiers" in (ROOT / ".github" / "pull_request_template.md").read_text()


def test_code_owners_cover_the_policy_files():
    owners = (ROOT / ".github" / "CODEOWNERS").read_text()
    for path in ("/SECURITY.md", "/TRADEMARK.md", "/.github/"):
        assert re.search(rf"(?m)^{re.escape(path)}\s+@UstDoesTech$", owners), path


def test_the_trademark_note_does_not_claim_to_be_legal_advice():
    assert "not legal advice" in (ROOT / "TRADEMARK.md").read_text()
