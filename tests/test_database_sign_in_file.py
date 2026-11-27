"""K-24e: the service reads how it signs in from the secret's `db-sign-in` file. The service's own
behaviour is proven in crates/api/tests/binary.rs; these hold the boot module and the docs."""
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BOOT = ROOT / "infra" / "modules" / "api-boot" / "main.tf"
MAIN = (ROOT / "crates" / "api" / "src" / "main.rs").read_text()


def tf(text):
    return "\n".join(l for l in text.splitlines() if not l.lstrip().startswith("#"))


def test_the_boot_module_lists_the_file_as_optional_and_points_the_service_and_the_migration_at_it():
    text = tf(BOOT.read_text())
    optional = re.search(r"optional_secret_files = \[(.*?)\]", text, re.S).group(1)
    assert '"db-sign-in"' in optional
    required = re.search(r"secret_files = \[(.*?)\]", text, re.S).group(1)
    assert "db-sign-in" not in required, "a cell that is applied has no such entry, so it cannot be required"
    env = re.search(r"environment = \{(.*?)\n  \}", text, re.S).group(1)
    assert 'LEDGER_DB_SIGN_IN_FILE    = "${local.secrets_dir}/db-sign-in"' in env
    migrate = re.search(r"migrate_environment = \{(.*?)\n  \}", text, re.S).group(1)
    assert "LEDGER_DB_SIGN_IN_FILE = local.environment.LEDGER_DB_SIGN_IN_FILE" in migrate


def test_every_cloud_gets_it_because_the_boot_module_is_shared():
    for cloud in ("aws", "azure", "gcp"):
        text = (ROOT / "infra" / "modules" / "api" / cloud / "main.tf").read_text()
        assert 'source = "../../api-boot"' in text, cloud


def test_the_service_reads_the_variable_first_then_the_file_and_defaults_to_the_password():
    fn = MAIN.split("fn sign_in_word()", 1)[1].split("\n}\n", 1)[0]
    assert fn.index("LEDGER_DB_SIGN_IN\"") < fn.index("LEDGER_DB_SIGN_IN_FILE")
    assert "ErrorKind::NotFound => Ok(None)" in fn and "Ok(_) => Ok(None)" in fn
    assert "None => ledger_api::dbauth::SignIn::Password" in MAIN
    # A word read from the file goes through the same parser, which refuses what it does not know.
    assert "SignIn::parse(&text)" in MAIN


def test_the_word_is_never_logged_or_returned():
    fn = MAIN.split("fn sign_in_word()", 1)[1].split("\n}\n", 1)[0]
    assert "println" not in fn and "eprintln" not in fn and "format!" not in fn.replace('"LEDGER_DB_SIGN_IN_FILE: cannot read it".to_owned()', "")


def test_the_docs_say_where_the_word_comes_from():
    design = (ROOT / "docs" / "api-design.md").read_text()
    assert "LEDGER_DB_SIGN_IN_FILE" in design
    assert "LEDGER_DB_SIGN_IN_FILE" in MAIN.split("use ", 1)[0], "described at the top of main.rs"
    creds = (ROOT / "docs" / "credentialed-workflows.md").read_text()
    section = creds.split("#### Managed cells (K-24d)", 1)[1].split("\n#", 1)[0]
    assert "K-24e" in section
