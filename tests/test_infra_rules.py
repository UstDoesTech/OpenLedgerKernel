"""Rules for the infrastructure code that CI can check without a cloud account."""
import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
INFRA = ROOT / "infra"
WORKFLOWS = ROOT / ".github" / "workflows"
CLOUDS = {
    "aws": "hashicorp/aws",
    "azurerm": "hashicorp/azurerm",
    "google": "hashicorp/google",
    "databricks": "databricks/databricks",  # I-01d: Lakebase
}


def code(path: Path) -> str:
    """The file without full-line comments, so rules can mention what they forbid."""
    return "\n".join(l for l in path.read_text().splitlines() if not l.lstrip().startswith("#"))


def test_infra_workflow_never_applies_and_holds_no_credentials():
    text = code(WORKFLOWS / "infra.yml")
    assert not re.search(r"\b(tofu|terraform)\s+(apply|destroy|import|plan|refresh)\b", text), "infra CI must only validate"
    assert "secrets." not in text, "infra CI must hold no secrets"
    assert not re.search(r"id-token\s*:\s*write", text), "infra CI must not assume cloud roles"
    assert re.search(r"permissions:\s*\n\s*contents:\s*read\s*\n", text), "token should be read-only"
    for name in ("AWS_ACCESS_KEY", "AWS_SECRET", "ARM_", "AZURE_", "GOOGLE_", "TF_VAR"):
        assert name not in text, name


def test_no_workflow_applies_infrastructure():
    for wf in WORKFLOWS.glob("*.yml"):
        assert not re.search(r"\b(tofu|terraform)\s+(apply|destroy)\b", code(wf)), f"{wf.name} would change real infrastructure"


def test_infra_workflow_checks_format_validity_and_lint_on_every_infra_change():
    text = code(WORKFLOWS / "infra.yml")
    # Runs on every pull request and every push to main that touches the infra code, and on
    # nothing else, so a change to infra can never skip it.
    triggers = text.split("on:", 1)[1].split("permissions:", 1)[0]
    assert re.search(r"push:\s*\n\s*branches: \[main\]\s*\n\s*paths:", triggers)
    assert re.search(r"pull_request:\s*\n\s*paths:", triggers)
    assert triggers.count('"infra/**"') == 2, "both triggers must cover everything under infra/"
    assert triggers.count('"scripts/check_uk_regions.py"') == 2
    assert "tofu fmt -check" in text
    assert "tofu" in text and "validate" in text
    assert "tflint" in text
    assert "check_uk_regions.py" in text


def test_no_state_secrets_or_provider_caches_are_committed():
    for path in INFRA.rglob("*"):
        if path.is_file():
            assert ".terraform" not in path.parts or path.name == ".terraform.lock.hcl", path
            assert ".tfstate" not in path.name, path
            if path.suffix in (".tf", ".tfvars", ".json", ".md", ".hcl"):
                text = path.read_text()
                assert not re.search(r"AKIA[0-9A-Z]{16}", text), f"{path} looks like it holds an AWS key"
                assert "-----BEGIN" not in text, f"{path} looks like it holds a private key"
                assert not re.search(r"(?i)(password|secret|token)\s*=\s*\"[^\"$]+\"", text), f"{path} has a literal secret"


def test_provider_pins_exist_for_all_three_clouds_and_databricks():
    pins = json.loads((INFRA / "providers.json").read_text())
    assert set(pins) == set(CLOUDS)
    for name, pin in pins.items():
        assert pin["source"] == CLOUDS[name]
        assert re.match(r"^(~>|=|>=)\s*\d+\.\d+", pin["version"]), f"{name} must be pinned, got {pin['version']}"


def test_every_module_uses_the_pinned_provider_versions():
    pins = json.loads((INFRA / "providers.json").read_text())
    for tf in INFRA.rglob("*.tf"):
        if ".terraform" in tf.parts:
            continue
        text = code(tf)
        for name, pin in pins.items():
            for block in re.finditer(rf"\b{name}\s*=\s*\{{([^}}]*)\}}", text):
                body = block.group(1)
                assert f'source  = "{pin["source"]}"' in body or f'source = "{pin["source"]}"' in body, f"{tf}: {name} source"
                assert pin["version"] in body, f"{tf}: {name} must use version {pin['version']}"


def test_modules_take_regions_from_the_regions_module_not_by_hand():
    # Resource modules (anything but regions) must not hard-code a region literal.
    guard_ok = re.compile(r'^\s*(region|location|zone|zones|availability_zones?)\s*=\s*"[a-z]', re.I)
    for tf in INFRA.rglob("*.tf"):
        if ".terraform" in tf.parts or "regions" in tf.parts:
            continue
        for no, line in enumerate(code(tf).splitlines(), 1):
            assert not guard_ok.match(line), f"{tf}:{no}: take the region from modules/regions"


def test_infra_ci_downloads_each_provider_once_before_it_initialises_anything():
    # Each directory runs `tofu init`. Without a shared cache every one downloads its own copy of
    # every provider, and the runner runs out of disk ("no space left on device").
    text = code(WORKFLOWS / "infra.yml")
    assert "TF_PLUGIN_CACHE_DIR" in text
    assert text.index("TF_PLUGIN_CACHE_DIR") < text.index("tofu -chdir")
