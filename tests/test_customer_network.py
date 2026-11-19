"""I-01j: running the Postgres image inside a customer's own network.

CI renders the Kubernetes manifests with kubectl and sets RENDERED_MANIFESTS to the result, so
these rules are checked on what a customer would apply. Without it they read the source files.
"""
import json
import os
import re
from pathlib import Path

import kubernetes_validate
import pytest
import yaml

ROOT = Path(__file__).resolve().parent.parent
K8S = ROOT / "deploy" / "kubernetes"
COMPOSE = ROOT / "deploy" / "compose"
GUIDE = ROOT / "docs" / "self-hosted-postgres.md"
FLOWS = json.loads((ROOT / "infra" / "flows.json").read_text())
K8S_VERSION = "1.34"

# The ports the Postgres node may use are the ones the flow policy gives to Postgres.
POSTGRES_PORT = FLOWS["ports"]["postgres"]
POSTGRES_FLOWS = [f for f in FLOWS["flows"] if f["port"] == "postgres"]


def source_docs() -> list[dict]:
    kustomization = yaml.safe_load((K8S / "kustomization.yaml").read_text())
    docs = []
    for name in kustomization["resources"]:
        docs += [d for d in yaml.safe_load_all((K8S / name).read_text()) if d]
    return docs


def docs() -> list[dict]:
    rendered = os.environ.get("RENDERED_MANIFESTS")
    if rendered:
        return [d for d in yaml.safe_load_all(Path(rendered).read_text()) if d]
    return source_docs()


def one(kind: str) -> dict:
    found = [d for d in docs() if d["kind"] == kind]
    assert len(found) == 1, f"expected one {kind}, found {len(found)}"
    return found[0]


def containers(sts: dict) -> list[dict]:
    spec = sts["spec"]["template"]["spec"]
    return spec.get("initContainers", []) + spec["containers"]


# ---- the manifests are valid ------------------------------------------------------------------


def test_every_manifest_is_valid_for_kubernetes():
    for doc in source_docs():
        kubernetes_validate.validate(doc, K8S_VERSION, strict=True)


def test_rendered_manifests_are_valid_and_in_one_namespace():
    if "RENDERED_MANIFESTS" not in os.environ:
        pytest.skip("CI renders the manifests with kubectl")
    for doc in docs():
        kubernetes_validate.validate(doc, K8S_VERSION, strict=True)
        assert doc["metadata"].get("namespace") == "ledger", doc["kind"]


def test_the_manifests_hold_no_secrets():
    text = "".join(p.read_text() for p in K8S.iterdir() if p.is_file())
    assert "PRIVATE KEY" not in text
    assert not re.search(r"(?i)password\s*[:=]\s*\S+", text)
    assert all(d["kind"] != "Secret" for d in docs()), "certificates are the customer's own"


# ---- three nodes, three zones -----------------------------------------------------------------


def test_three_nodes_spread_one_to_a_zone_and_never_two_to_a_machine():
    sts = one("StatefulSet")["spec"]
    assert sts["replicas"] == 3
    spread = sts["template"]["spec"]["topologySpreadConstraints"]
    assert any(
        c["topologyKey"] == "topology.kubernetes.io/zone" and c["maxSkew"] == 1 and c["whenUnsatisfiable"] == "DoNotSchedule"
        for c in spread
    )
    anti = sts["template"]["spec"]["affinity"]["podAntiAffinity"]["requiredDuringSchedulingIgnoredDuringExecution"]
    assert any(t["topologyKey"] == "kubernetes.io/hostname" for t in anti)


def test_a_manifest_change_never_restarts_a_database():
    assert one("StatefulSet")["spec"]["updateStrategy"]["type"] == "OnDelete"


def test_a_drain_takes_one_node_at_a_time():
    pdb = one("PodDisruptionBudget")["spec"]
    assert pdb.get("maxUnavailable") == 1 or pdb.get("minAvailable") == 2


def test_the_service_is_headless_and_names_the_pods():
    service = one("Service")["spec"]
    assert service["clusterIP"] == "None"
    assert one("StatefulSet")["spec"]["serviceName"] == one("Service")["metadata"]["name"]
    assert [p["port"] for p in service["ports"]] == [POSTGRES_PORT]


def test_each_node_has_its_own_encrypted_volume_class_and_a_credential_volume():
    claims = {c["metadata"]["name"]: c["spec"] for c in one("StatefulSet")["spec"]["volumeClaimTemplates"]}
    assert set(claims) == {"data", "credential"}
    for spec in claims.values():
        assert spec["accessModes"] == ["ReadWriteOnce"]
        assert spec["storageClassName"], "the customer names a class that encrypts with their key"


# ---- the pod is locked down -------------------------------------------------------------------


def test_no_container_is_privileged_or_writable_or_root():
    spec = one("StatefulSet")["spec"]["template"]["spec"]
    assert spec["securityContext"]["runAsNonRoot"] is True
    assert spec["securityContext"]["runAsUser"] == 999
    assert spec["securityContext"]["seccompProfile"]["type"] == "RuntimeDefault"
    assert spec["automountServiceAccountToken"] is False
    for key in ("hostNetwork", "hostPID", "hostIPC"):
        assert not spec.get(key), key
    assert not any("hostPath" in v for v in spec["volumes"])
    for c in containers(one("StatefulSet")):
        ctx = c["securityContext"]
        assert ctx["allowPrivilegeEscalation"] is False, c["name"]
        assert ctx["readOnlyRootFilesystem"] is True, c["name"]
        assert ctx["capabilities"]["drop"] == ["ALL"], c["name"]
        assert not ctx.get("privileged"), c["name"]


def test_the_certificates_come_from_a_secret_the_customer_makes():
    volumes = {v["name"]: v for v in one("StatefulSet")["spec"]["template"]["spec"]["volumes"]}
    assert volumes["tls"]["secret"]["secretName"] == "ledger-pg-tls"
    mount = next(m for m in one("StatefulSet")["spec"]["template"]["spec"]["containers"][0]["volumeMounts"] if m["name"] == "tls")
    assert mount["mountPath"] == "/run/secrets/tls" and mount["readOnly"] is True


def test_the_image_is_a_placeholder_the_customer_points_at_their_registry():
    for c in containers(one("StatefulSet")):
        assert re.match(r"^(ledger-postgres|[^:@]+/ledger-postgres)([:@].+)?$", c["image"]), c["image"]
    if "RENDERED_MANIFESTS" not in os.environ:
        kustomization = yaml.safe_load((K8S / "kustomization.yaml").read_text())
        assert kustomization["images"][0]["name"] == "ledger-postgres"


def test_every_config_map_the_pod_uses_exists():
    sts = one("StatefulSet")["spec"]["template"]["spec"]
    used = {v["configMap"]["name"] for v in sts["volumes"] if "configMap" in v}
    for c in containers(one("StatefulSet")):
        used |= {e["configMapRef"]["name"] for e in c.get("envFrom", [])}
    if "RENDERED_MANIFESTS" in os.environ:
        have = {d["metadata"]["name"] for d in docs() if d["kind"] == "ConfigMap"}
    else:
        kustomization = yaml.safe_load((K8S / "kustomization.yaml").read_text())
        have = {g["name"] for g in kustomization["configMapGenerator"]}
    assert used and used <= have, (used, have)


# ---- the network policy ----------------------------------------------------------------------


def test_the_policy_lets_in_only_the_postgres_port_from_the_nodes_and_the_platform():
    policy = one("NetworkPolicy")["spec"]
    assert set(policy["policyTypes"]) == {"Ingress", "Egress"}
    assert len(policy["ingress"]) == 1
    ports = {(p["protocol"], p["port"]) for p in policy["ingress"][0]["ports"]}
    assert ports == {("TCP", POSTGRES_PORT)}
    assert POSTGRES_FLOWS, "the flow policy names the postgres flows this serves"
    sources = policy["ingress"][0]["from"]
    assert any(s.get("podSelector", {}).get("matchLabels") == {"app": "ledger-pg"} for s in sources)
    cidrs = [s["ipBlock"]["cidr"] for s in sources if "ipBlock" in s]
    assert cidrs and all(c != "0.0.0.0/0" and not c.endswith("/0") for c in cidrs)
    assert cidrs == ["192.0.2.0/24"], "a document range until the customer sets the platform's ranges"


def test_the_policy_lets_out_only_the_peers_and_name_lookups():
    egress = one("NetworkPolicy")["spec"]["egress"]
    seen = set()
    for rule in egress:
        for p in rule["ports"]:
            seen.add((p["protocol"], p["port"]))
    assert seen == {("TCP", POSTGRES_PORT), ("UDP", 53), ("TCP", 53)}
    assert not any("ipBlock" in t for rule in egress for t in rule["to"]), "nothing can reach out to an address range"


def test_the_policy_opens_none_of_the_other_platform_ports():
    others = {v for k, v in FLOWS["ports"].items() if k != "postgres"}
    text = (K8S / "networkpolicy.yaml").read_text()
    for port in others:
        assert not re.search(rf"port:\s*{port}\b", text), port


# ---- the init script: who is the primary ------------------------------------------------------


def test_the_primary_is_a_setting_and_the_old_primary_is_made_a_standby():
    script = (K8S / "render-config.sh").read_text()
    assert "PRIMARY_ORDINAL" in script
    kustomization = yaml.safe_load((K8S / "kustomization.yaml").read_text())
    literals = kustomization["configMapGenerator"][1]["literals"]
    assert "PRIMARY_ORDINAL=0" in literals


# ---- compose ----------------------------------------------------------------------------------


def compose_services() -> dict:
    return yaml.safe_load((COMPOSE / "compose.yaml").read_text())["services"]


def test_compose_runs_three_nodes_locked_down():
    services = compose_services()
    assert set(services) == {"pg1", "pg2", "pg3"}
    for name, s in services.items():
        assert s["read_only"] is True, name
        assert s["cap_drop"] == ["ALL"], name
        assert "no-new-privileges:true" in s["security_opt"], name
        assert s["stop_signal"] == "SIGINT", name
        assert not s.get("privileged"), name
        assert s.get("network_mode") != "host", name
        assert s["image"].startswith("${LEDGER_IMAGE:?"), "the image is chosen by the customer"
        for port in s["ports"]:
            assert port.startswith("127.0.0.1:"), "ports are never bound to every address"
            assert port.endswith(f":{POSTGRES_PORT}"), port
        assert not any(v.startswith(("/", "~")) for v in s["volumes"]), "no host paths but the config and certificates"


def test_compose_mounts_config_and_certificates_read_only_and_keeps_data_in_volumes():
    for name, s in compose_services().items():
        mounts = {v.split(":")[1]: v for v in s["volumes"]}
        assert mounts["/etc/ledger/node.json"].endswith(":ro")
        assert mounts["/run/secrets/tls"].endswith(":ro")
        assert mounts["/var/lib/postgresql/data"].startswith(f"{name}-data:")
        assert mounts["/secrets/out"].startswith(f"{name}-credential:")


def test_compose_configurations_match_the_services():
    for n in (1, 2, 3):
        config = json.loads((COMPOSE / "config" / f"pg{n}.json").read_text())
        assert config["node"] == f"pg{n}"
        assert config["role"] == ("primary" if n == 1 else "standby")


def test_no_key_or_certificate_is_committed():
    for path in (ROOT / "deploy").rglob("*"):
        if path.is_file():
            assert path.suffix not in {".key", ".pem", ".crt", ".csr"}, path
            assert "PRIVATE KEY" not in path.read_text(errors="ignore"), path
    assert "tls/" in (COMPOSE / ".gitignore").read_text()


def test_the_test_certificates_are_short_lived_and_named_for_the_agent():
    script = (COMPOSE / "make-test-certs.sh").read_text()
    assert "-days 2" in script
    assert "issue replication replicator" in script
    assert "Never use them for real data" in script


# ---- the guide --------------------------------------------------------------------------------


def test_the_guide_lists_what_the_customer_provides_and_says_failover_is_manual():
    text = GUIDE.read_text()
    lowered = text.lower()
    for needed in ("private registry", "three zones", "encrypted", "certificates", "secret store"):
        assert needed in lowered, needed
    assert re.search(r"failover is manual", lowered)
    assert "the business" not in lowered
    for linked in re.findall(r"\(((?:\.\./|\./)[^)#]+)", text):
        assert (GUIDE.parent / linked).exists(), f"broken link {linked}"
