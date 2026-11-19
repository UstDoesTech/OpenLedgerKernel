#!/usr/bin/env python3
"""Fail if any OpenTofu configuration uses a region outside the geo it belongs to.

    python scripts/check_uk_regions.py [PATH ...]      # default: infra/

(The file keeps its first name. It guards every geo, not only the UK.)

Policy lives in one file, infra/geos.json:

    {"enabled": ["uk"], "geos": {"uk": {"aws": [...], "azure": [...], "gcp": [...]}}}

Every region listed for a geo is allowed in that geo. Only enabled geos may be used. Adding a
geo, or enabling one, is a change to that file, reviewed by a person. The guard itself does
not change.

Scans .tf and .tfvars files. Rules:

1. Each environment (infra/envs/<name>/) names its geo in deployment.json, and that geo must
   be enabled. Its files may only use that geo's regions. Files outside envs/ (modules) may
   use the regions of any enabled geo, and take their region from a variable in practice.
2. Any quoted value that looks like an AWS or Google Cloud region (or zone), or is a known
   Azure region, must be an allowed region.
3. Any attribute with the word "region" or "location" in its name and a value that looks like
   a region must be an allowed region. This catches Azure regions the lists miss. A word is
   a run of letters between underscores, hyphens, dots or capitals, so `location`,
   `source_region` and `gatewayLocation` count, and `allocation_method` and `relocation` do
   not.
4. Multi-regions that span countries (Google "EU", "US", "ASIA" and the dual-regions) are
   never allowed.
5. The policy file is checked: names are known, regions are well formed, and no region is in
   two geos.

Add `# uk-guard: ignore` to a line to exempt it, with a reason beside it. Values built from
variables (`"${var.x}"`) cannot be checked here, so they should come from modules/regions.
"""
from __future__ import annotations

import json
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CLOUDS = ("aws", "azure", "gcp")

AWS_RE = re.compile(r"^(af|ap|ca|cn|eu|il|me|mx|sa|us)(-gov|-iso[a-z]?)?-[a-z]+-\d(?P<zone>[a-z])?$")
GCP_RE = re.compile(
    r"^(africa|asia|australia|europe|me|northamerica|southamerica|us)-[a-z]+\d+(?P<zone>-[a-z])?$"
)
# Azure regions the guard knows by name, normalised (lower case, no spaces or hyphens). Not
# exhaustive: rule 3 catches the rest when they are used as a location or region.
AZURE_KNOWN = set(
    """uksouth ukwest
    eastus eastus2 westus westus2 westus3 centralus northcentralus southcentralus westcentralus
    canadacentral canadaeast brazilsouth brazilsoutheast mexicocentral chilecentral
    northeurope westeurope francecentral francesouth germanywestcentral germanynorth
    switzerlandnorth switzerlandwest norwayeast norwaywest swedencentral swedensouth polandcentral
    italynorth spaincentral austriaeast uaenorth uaecentral qatarcentral israelcentral
    southafricanorth southafricawest australiaeast australiasoutheast australiacentral
    australiacentral2 centralindia southindia westindia jioindiawest jioindiacentral japaneast
    japanwest koreacentral koreasouth southeastasia eastasia indonesiacentral malaysiawest
    newzealandnorth chinanorth chinanorth2 chinanorth3 chinaeast chinaeast2 chinaeast3
    usgovvirginia usgovarizona usgovtexas usdodeast usdodcentral""".split()
)
# Google multi-regions and dual-regions span countries; none is confined to one geo.
MULTI_REGIONS = {"eu", "us", "asia", "nam4", "eur4", "eur5", "eur7", "asia1"}

IGNORE = "uk-guard: ignore"
ATTR_RE = re.compile(r"^\s*([A-Za-z0-9_.-]+)\s*=\s*(.*)$")
NAME_WORD = re.compile(r"[A-Z]?[a-z0-9]+|[A-Z]+(?![a-z])")
REGION_WORDS = {"region", "regions", "location", "locations"}
STR_RE = re.compile(r'"((?:[^"\\]|\\.)*)"')
REGION_SHAPED = re.compile(r"^[A-Za-z][A-Za-z0-9 -]{2,40}$")
GEO_NAME = re.compile(r"^[a-z][a-z0-9-]{1,30}$")
AZURE_NAME = re.compile(r"^[a-z0-9]{3,40}$")


def names_a_region(attribute: str) -> bool:
    """True if the attribute name has the word region or location in it (rule 3)."""
    return any(w.lower() in REGION_WORDS for w in NAME_WORD.findall(attribute))


def norm(value: str) -> str:
    return re.sub(r"[\s-]", "", value.lower())


@dataclass
class Policy:
    enabled: list[str] = field(default_factory=list)
    geos: dict[str, dict[str, set[str]]] = field(default_factory=dict)

    def allowed(self, geo: str) -> dict[str, set[str]]:
        return {c: set(self.geos[geo].get(c, ())) for c in CLOUDS}

    def allowed_anywhere(self) -> dict[str, set[str]]:
        merged: dict[str, set[str]] = {c: set() for c in CLOUDS}
        for geo in self.enabled:
            for c, regions in self.allowed(geo).items():
                merged[c] |= regions
        return merged


def load_policy(path: Path) -> tuple[Policy, list[str]]:
    """Read and check the policy file. Always returns a usable Policy, plus any errors."""
    try:
        data = json.loads(path.read_text())
    except (OSError, ValueError) as e:
        return Policy(), [f"{path}: cannot read: {e}"]
    errors: list[str] = []
    policy = Policy()
    if not isinstance(data, dict) or set(data) != {"enabled", "geos"}:
        return policy, [f'{path}: must hold exactly "enabled" and "geos"']
    geos, enabled = data["geos"], data["enabled"]
    if not isinstance(geos, dict) or not geos:
        return policy, [f'{path}: "geos" must be an object with at least one geo']
    owner: dict[tuple[str, str], str] = {}
    for geo, clouds in geos.items():
        if not GEO_NAME.match(geo):
            errors.append(f"{path}: bad geo name {geo!r}")
            continue
        if not isinstance(clouds, dict) or set(clouds) != set(CLOUDS):
            errors.append(f"{path}: geo {geo!r} must list exactly {', '.join(CLOUDS)}")
            continue
        policy.geos[geo] = {}
        for cloud in CLOUDS:
            regions = clouds[cloud]
            if not isinstance(regions, list) or not regions or not all(isinstance(r, str) for r in regions):
                errors.append(f"{path}: {geo}.{cloud} must be a non-empty list of region names")
                continue
            ok = set()
            for r in regions:
                if not _well_formed(cloud, r):
                    errors.append(f"{path}: {geo}.{cloud}: {r!r} is not a region name for {cloud}")
                elif (cloud, norm(r)) in owner and owner[(cloud, norm(r))] != geo:
                    errors.append(f"{path}: {cloud} region {r!r} is in both {owner[(cloud, norm(r))]!r} and {geo!r}")
                else:
                    owner[(cloud, norm(r))] = geo
                    ok.add(r)
            policy.geos[geo][cloud] = ok
    if not isinstance(enabled, list) or not enabled or not all(isinstance(g, str) for g in enabled):
        errors.append(f'{path}: "enabled" must be a non-empty list of geo names')
    else:
        for g in enabled:
            if g not in policy.geos:
                errors.append(f"{path}: enabled geo {g!r} is not defined in geos")
        policy.enabled = [g for g in enabled if g in policy.geos]
    return policy, errors


def _well_formed(cloud: str, region: str) -> bool:
    if cloud == "aws":
        m = AWS_RE.match(region)
        return bool(m) and not m.group("zone")
    if cloud == "gcp":
        m = GCP_RE.match(region)
        return bool(m) and not m.group("zone")
    return bool(AZURE_NAME.match(norm(region)))


def strip_comments(text: str) -> list[str]:
    """Return the lines with comments blanked, keeping line numbers. Handles #, // and
    /* */, and leaves anything inside a quoted string alone."""
    out: list[str] = []
    in_block = False
    for raw in text.splitlines():
        line, i, buf, in_str = raw, 0, [], False
        while i < len(line):
            c = line[i]
            if in_block:
                if line.startswith("*/", i):
                    in_block, i = False, i + 2
                else:
                    i += 1
                continue
            if in_str:
                buf.append(c)
                if c == "\\" and i + 1 < len(line):
                    buf.append(line[i + 1])
                    i += 2
                    continue
                if c == '"':
                    in_str = False
                i += 1
                continue
            if c == '"':
                in_str = True
            elif c == "#" or line.startswith("//", i):
                break
            elif line.startswith("/*", i):
                in_block, i = True, i + 2
                continue
            buf.append(c)
            i += 1
        out.append("".join(buf))
    return out


def region_problem(value: str, allowed: dict[str, set[str]], context: str, label: str = "an enabled geo") -> str | None:
    """Return why `value` is not an allowed region, or None if it is fine or not a region."""
    if "${" in value or "%{" in value:
        return None  # built from something else; cannot be checked here
    everything = set().union(*allowed.values())
    allowed_norm = {norm(r) for r in everything}
    n = norm(value)
    if n == "global":
        return None  # a global resource has no region of its own
    zone_of = None
    m = AWS_RE.match(value)
    if m:
        zone_of = value[:-1] if m.group("zone") else value
        looks_like = True
    else:
        g = GCP_RE.match(value)
        looks_like = bool(g)
        if g:
            zone_of = value[: -len(g.group("zone"))] if g.group("zone") else value
    if looks_like:
        return None if zone_of in everything else f"{value!r} is not an allowed region for {label}"
    if n in AZURE_KNOWN and n not in allowed_norm:
        return f"{value!r} is an Azure region outside {label}"
    if context == "attribute" and n in MULTI_REGIONS:
        return f"{value!r} is a multi-region that spans countries and geos"
    if context == "attribute" and REGION_SHAPED.match(value) and n not in allowed_norm:
        return f"{value!r} is not an allowed region for {label}"
    return None


def scan_text(text: str, allowed: dict[str, set[str]], label: str = "an enabled geo") -> list[tuple[int, str]]:
    problems: list[tuple[int, str]] = []
    raw_lines = text.splitlines()
    for no, line in enumerate(strip_comments(text), start=1):
        if IGNORE in raw_lines[no - 1]:
            continue
        seen: set[str] = set()
        m = ATTR_RE.match(line)
        if m and names_a_region(m.group(1)):
            for s in STR_RE.findall(m.group(2)):
                why = region_problem(s, allowed, "attribute", label)
                if why:
                    problems.append((no, f"{m.group(1)} = {why}"))
                    seen.add(s)
        for s in STR_RE.findall(line):
            if s in seen:
                continue
            why = region_problem(s, allowed, "literal", label)
            if why:
                problems.append((no, why))
    return problems


def environment_of(rel: tuple[str, ...]) -> tuple[str | None, bool]:
    """For a path relative to the scanned root: (environment name, loose) where `loose` means
    the file sits directly in envs/ and so belongs to no environment."""
    if "envs" not in rel:
        return None, False
    rest = rel[rel.index("envs") + 1 :]
    if len(rest) >= 2:
        return rest[0], False
    return None, True


def read_deployment(env_dir: Path, policy: Policy) -> tuple[str | None, list[str]]:
    f = env_dir / "deployment.json"
    try:
        data = json.loads(f.read_text())
    except OSError:
        return None, [f'{env_dir}: add deployment.json naming its geo, for example {{"geo": "uk"}}']
    except ValueError as e:
        return None, [f"{f}: not valid JSON: {e}"]
    if not isinstance(data, dict) or set(data) != {"geo"} or not isinstance(data["geo"], str):
        return None, [f'{f}: must be exactly {{"geo": "<name>"}}']
    geo = data["geo"]
    if geo not in policy.geos:
        return None, [f"{f}: geo {geo!r} is not defined in geos.json"]
    if geo not in policy.enabled:
        return None, [f"{f}: geo {geo!r} is not enabled in geos.json"]
    return geo, []


def main(argv: list[str]) -> int:
    paths = [Path(a) for a in argv] or [ROOT / "infra"]
    entries: list[tuple[Path, tuple[str, ...], Path]] = []  # file, path relative to its root, root
    for p in paths:
        if p.is_dir():
            entries += [(f, f.relative_to(p).parts, p) for f in sorted(p.rglob("*"))]
        else:
            entries.append((p, (p.name,), p.parent))
    entries = [e for e in entries if e[0].is_file() and ".terraform" not in e[0].parts]

    errors: list[str] = []
    policy_files = [e[0] for e in entries if e[0].name == "geos.json"] or [ROOT / "infra" / "geos.json"]
    policy, policy_errors = load_policy(policy_files[0])
    errors += policy_errors
    for extra in policy_files[1:]:
        errors += load_policy(extra)[1]

    anywhere = policy.allowed_anywhere()
    env_geo: dict[Path, str | None] = {}
    scanned = 0
    for f, rel, root in entries:
        env, loose = environment_of(rel)
        is_config = f.suffix in (".tf", ".tfvars")
        env_dir = None
        if env is not None:
            env_dir = root.joinpath(*rel[: rel.index("envs") + 2])
            if env_dir not in env_geo:
                env_geo[env_dir], more = read_deployment(env_dir, policy)
                errors += more
        if f.name.endswith(".tf.json") or f.name.endswith(".tfvars.json"):
            errors.append(f"{f}: JSON configuration is not scanned by the region guard; use .tf")
            continue
        if not is_config:
            continue
        if loose:
            errors.append(f"{f}: configuration directly in envs/ names no geo; put it in envs/<name>/")
            continue
        scanned += 1
        if env_dir is not None:
            geo = env_geo[env_dir]
            if geo is None:
                continue  # reported above
            allowed, label = policy.allowed(geo), f"geo {geo!r}"
        else:
            allowed, label = anywhere, "the enabled geos"
        for no, why in scan_text(f.read_text(), allowed, label):
            errors.append(f"{f}:{no}: {why}")

    for e in errors:
        print(e, file=sys.stderr)
    if errors:
        print(f"Region guard: {len(errors)} problem(s)", file=sys.stderr)
        return 1
    print(f"Region guard: {scanned} file(s) checked, enabled geos: {', '.join(policy.enabled)}; every region is inside its geo")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
