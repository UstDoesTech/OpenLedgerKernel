"""AWS refuses a security group rule whose description has a character outside a small set (an
apostrophe is one). The module tests cannot see that, because only AWS checks it when the rule is
made, so this checks every AWS security group and rule description under infra/ against the set."""
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ALLOWED = re.compile(r"^[a-zA-Z0-9. _\-:/()#,@\[\]+=&;{}!$*]{0,255}$")
KINDS = r"aws_(?:vpc_security_group_(?:ingress|egress)_rule|security_group_rule|security_group)"
BLOCK = re.compile(r'resource "%s" "[^"]+" \{(.*?)\n\}' % KINDS, re.S)
DESCRIPTION = re.compile(r'\n\s*description\s*=\s*"([^"]*)"')


def descriptions():
    for path in sorted((ROOT / "infra").rglob("*.tf")):
        for block in BLOCK.finditer(path.read_text()):
            for d in DESCRIPTION.finditer(block.group(1)):
                yield path.relative_to(ROOT), d.group(1)


def test_every_aws_security_group_and_rule_description_is_one_aws_accepts():
    found = list(descriptions())
    assert found, "the check found no descriptions, so it checks nothing"
    bad = [(str(p), d) for p, d in found if not ALLOWED.match(d)]
    assert not bad, bad


def test_the_check_would_catch_an_apostrophe():
    assert not ALLOWED.match("Allow the API's port")
    assert ALLOWED.match("Allow the port of the API, from the load balancer (443)")
