"""K-23d: the limits on sign-in are per address, and no address, username or password is logged.

The behaviour is tested in Rust (crates/api: unit tests and tests/sign_in.rs). These checks hold
what a Rust test cannot see: that the code that handles the address makes no log line, and that the
documents say what the code does."""
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
API = ROOT / "crates" / "api" / "src"
LOG = re.compile(r"\b(tracing::|info!|warn!|error!|debug!|trace!|println!|eprintln!|dbg!|log::)")


def code(path):
    text = path.read_text()
    text = re.sub(r"(?m)^\s*//.*$", "", text)
    return text[: text.index("#[cfg(test)]")] if "#[cfg(test)]" in text else text


def test_the_code_that_handles_addresses_and_passwords_makes_no_log_line():
    for name in ("login.rs", "client_addr.rs"):
        assert not LOG.search(code(API / name)), name
    app = (API / "app.rs").read_text()
    handler = app[app.index("async fn sign_in<"):]
    handler = handler[: handler.index("\n}\n")]
    assert not LOG.search(handler)


def test_the_address_is_never_put_in_a_response_or_a_header():
    app = (API / "app.rs").read_text()
    handler = app[app.index("async fn sign_in<"):]
    handler = handler[: handler.index("\n}\n")]
    assert "client" not in handler[handler.index("StatusCode::CREATED"):]
    assert "x-forwarded-for" not in handler[handler.index("StatusCode::CREATED"):]


def test_the_forwarded_header_is_read_only_through_the_trusted_ranges():
    app = (API / "app.rs").read_text()
    assert app.count("x-forwarded-for") == 1
    assert "trusted_proxies.client(peer, &forwarded)" in app
    assert "TrustedProxies::none()" in app, "nothing is trusted unless the service is told"
    main = (API / "main.rs").read_text()
    assert "LEDGER_TRUSTED_PROXIES" in main


def test_a_trusted_range_may_not_be_the_whole_internet():
    assert "may not be the whole internet" in (API / "client_addr.rs").read_text()


def test_the_documents_say_the_lock_is_per_address():
    design = (ROOT / "docs" / "api-design.md").read_text()
    spec = (ROOT / "api" / "openapi.yaml").read_text()
    for text in (design, spec):
        assert "one address for one username" in text
        assert "not locked" in text
    assert "LEDGER_TRUSTED_PROXIES" in design
