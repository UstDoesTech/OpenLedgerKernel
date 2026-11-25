"""K-24a: the service signs in to a managed database as its machine's identity (D-24).

The behaviour is tested in Rust (crates/api: unit tests, tests/token_sources.rs against recorded
answers, tests/db_identity.rs against a real Postgres node, tests/binary.rs for the start-up
refusals). These checks hold what a Rust test cannot see: a second implementation of the AWS
signature, that no code that handles a token makes a log line or prints a secret, and that the
documents say what the code does."""
import hashlib
import hmac
import datetime
import re
from pathlib import Path
from urllib.parse import quote

ROOT = Path(__file__).resolve().parent.parent
API = ROOT / "crates" / "api" / "src"
LOG = re.compile(r"\b(tracing::|info!|warn!|error!|debug!|trace!|println!|eprintln!|dbg!|log::)")


def code(path):
    text = path.read_text()
    text = re.sub(r"(?m)^\s*//.*$", "", text)
    return text[: text.index("#[cfg(test)]")] if "#[cfg(test)]" in text else text


def h(label):
    return hashlib.sha256(label.encode()).hexdigest()


def rds_token(host, port, user, region, akid, secret, session, now, expires=900):
    """AWS Signature Version 4 for an RDS authentication token, written without the Rust code."""
    t = datetime.datetime.fromtimestamp(now, datetime.timezone.utc)
    date, amz = t.strftime("%Y%m%d"), t.strftime("%Y%m%dT%H%M%SZ")
    scope = f"{date}/{region}/rds-db/aws4_request"
    q = {
        "Action": "connect",
        "DBUser": user,
        "X-Amz-Algorithm": "AWS4-HMAC-SHA256",
        "X-Amz-Credential": f"{akid}/{scope}",
        "X-Amz-Date": amz,
        "X-Amz-Expires": str(expires),
        "X-Amz-SignedHeaders": "host",
    }
    if session:
        q["X-Amz-Security-Token"] = session
    query = "&".join(
        f"{quote(k, safe='-_.~')}={quote(v, safe='-_.~')}" for k, v in sorted(q.items())
    )
    canon = f"GET\n/\n{query}\nhost:{host}:{port}\n\nhost\n{hashlib.sha256(b'').hexdigest()}"
    sts = f"AWS4-HMAC-SHA256\n{amz}\n{scope}\n{hashlib.sha256(canon.encode()).hexdigest()}"
    k = ("AWS4" + secret).encode()
    for part in (date, region, "rds-db", "aws4_request"):
        k = hmac.new(k, part.encode(), hashlib.sha256).digest()
    sig = hmac.new(k, sts.encode(), hashlib.sha256).hexdigest()
    return f"{host}:{port}/?{query}&X-Amz-Signature={sig}"


def test_the_rds_token_the_rust_code_makes_is_the_one_a_second_implementation_makes():
    source = (API / "dbauth.rs").read_text()
    block = source[source.index("let expected = concat!("):]
    block = block[: block.index(");")]
    pinned = "".join(re.findall(r'"([^"]*)"', block))
    akid = "AKIA" + h("akid")[:16].upper()
    want = rds_token(
        "db1.example.eu-west-2.rds.amazonaws.com", 5432, "ledger_api", "eu-west-2",
        akid, h("secret")[:40], h("session"), 1_800_000_000,
    )
    assert pinned == want


def test_the_code_that_handles_tokens_makes_no_log_line_and_prints_nothing():
    assert not LOG.search(code(API / "dbauth.rs"))
    conn = (API / "dbconn.rs").read_text()
    for start in ("pub struct Identity", "pub fn keep_connected"):
        part = conn[conn.index(start):]
        part = part[: part.index("\n}\n")]
        assert not LOG.search(part), start
    # The only line printed on a stop is the one the service's own main prints.
    main = (API / "main.rs").read_text()
    stop = main[main.index("keep_connected("):]
    assert stop[: stop.index("});")].count("eprintln!") == 1


def test_no_type_that_holds_a_secret_can_be_printed_by_a_derive():
    text = code(API / "dbauth.rs")
    for name in ("Token", "AwsCredentials", "LakebaseOauth", "AwsIam", "Identity"):
        m = re.search(r"((?:#\[[^\]]*\]\s*)*)pub struct %s\b" % name, text)
        if m:
            assert "Debug" not in m.group(1), name
    assert 'f.write_str("Token(..)")' in text


def test_a_failure_is_one_of_three_fixed_phrases_that_quote_nothing():
    text = code(API / "dbauth.rs")
    enum = text[text.index("pub enum TokenError"):]
    enum = enum[: enum.index("\n}\n")]
    assert len(re.findall(r"^\s+[A-Z]\w+,$", enum, re.M)) == 3
    texts = text[text.index("impl TokenError"):]
    texts = texts[: texts.index("\n}\n")]
    assert "format!" not in texts


def test_plain_http_goes_only_to_this_machine_and_the_metadata_addresses():
    text = code(API / "dbauth.rs")
    allowed = text[text.index("fn plain_http_allowed"):]
    allowed = allowed[: allowed.index("\n}\n")]
    hosts = set(re.findall(r'"([^"]+)"', allowed))
    assert hosts == {"127.0.0.1", "localhost", "[::1]", "169.254.169.254", "metadata.google.internal"}


def test_a_typo_in_the_setting_is_refused_and_does_not_fall_back_to_a_password():
    text = code(API / "dbauth.rs")
    parse = text[text.index("pub fn parse"):]
    parse = parse[: parse.index("\n    }\n")]
    assert re.findall(r'"(\w+)" =>', parse) == [
        "password", "aws_iam", "azure_entra", "gcp_iam", "lakebase_oauth"
    ]
    assert "_ => Err(" in parse


def test_the_documents_say_what_the_code_does():
    design = (ROOT / "docs" / "api-design.md").read_text()
    assert "### How the service signs in to its own database" in design
    for word in ("password", "aws_iam", "azure_entra", "gcp_iam", "lakebase_oauth",
                 "LEDGER_DB_SIGN_IN", "LEDGER_DB_OAUTH_FILE"):
        assert word in design, word
    main = (API / "main.rs").read_text()
    for name in ("LEDGER_DB_SIGN_IN", "LEDGER_DB_REGION", "LEDGER_DB_IDENTITY_CLIENT_ID",
                 "LEDGER_DB_OAUTH_FILE"):
        assert main.count(name) >= 2, name  # described at the top, and read


def test_the_default_is_the_password_so_a_cell_that_is_applied_does_not_change():
    main = (API / "main.rs").read_text()
    assert "Err(_) => ledger_api::dbauth::SignIn::Password" in main
    # The infrastructure does not set the new variable yet. That is K-24b's work.
    boot = (ROOT / "infra" / "modules" / "api-boot" / "main.tf").read_text()
    assert "LEDGER_DB_SIGN_IN" not in boot
