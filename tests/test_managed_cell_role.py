"""K-24ca: the pieces of the managed cell's application role that Python can hold. The statements
themselves are proven against a real node in crates/pki/tests/application_role.rs."""
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SRC = (ROOT / "crates" / "api" / "src" / "application_role.rs").read_text()
DOC = (ROOT / "docs" / "credentialed-workflows.md").read_text()
MAIN = (ROOT / "crates" / "pki" / "src" / "main.rs").read_text()


def code(text: str) -> str:
    return "\n".join(l for l in text.splitlines() if not l.lstrip().startswith("//"))


def test_the_role_is_made_with_no_password_and_no_powers():
    body = code(SRC)
    assert "nosuperuser nocreatedb nocreaterole noinherit" in body
    assert "nobypassrls noreplication" in body and "password null" in body
    # The only password the module names is the one it clears.
    assert not re.search(r"password\s+'", body)


def test_the_role_is_tied_to_the_identity_and_owns_only_the_ledger_schema():
    body = code(SRC)
    assert 'grant rds_iam to' in body
    assert 'create schema if not exists' in body and 'alter schema' in body and "owner to" in body
    assert "grant all" not in body.lower() and "superuser;" not in body


def test_the_step_is_a_command_a_person_runs_and_no_workflow_calls_it_yet():
    assert 'Some("managed-role")' in MAIN
    for wf in (ROOT / ".github" / "workflows").glob("*.yml"):
        assert "managed-role" not in wf.read_text(), wf.name


def test_the_doc_describes_it_in_the_files_that_exist():
    section = DOC.split("### 7. The application role of a managed cell", 1)[1].split("\n## ", 1)[0]
    assert "ledger-pki managed-role" in section
    assert "managed role: statements run: 10, failed: 0" in section
    for secret in ("db.conn", "tokens.json"):
        assert secret not in section
