# Infrastructure

OpenTofu for three clouds, AWS, Azure and Google Cloud, deployed by geo. Two geos are enabled:

- **`uk`**: AWS London (`eu-west-2`), Azure UK South and UK West, and Google Cloud London
  (`europe-west2`). This is the production rule: customer and ledger data stays in the UK.
- **`europe`**: the north Europe region of each cloud, AWS `eu-north-1` (Stockholm), Azure
  `northeurope` (Dublin) and Google Cloud `europe-north1` (Finland). It exists to **prove the
  platform** (failover drills, latency, the infrastructure workflow) because the accounts could
  be made there. **No real customer or ledger data goes in it.** The three regions are in
  different countries, so cross-cloud latency is higher than the London design assumes; latency
  measured here says nothing about the UK.

Layered OpenTofu roots, one per concern, with the rules in `tests/test_envs.py` and `tests/test_infra_rules.py`.

```
infra/
  geos.json           the geos, the regions in each, and which geos are enabled
  address_plan.json   one address range per cloud and geo, and how each is split into subnets
  links.json          BGP numbers and the link-local range inside each tunnel
  flows.json          the only traffic allowed between tiers, and its ports
  providers.json      the pinned provider versions every module must use
  .tflint.hcl         lint rules
  modules/            reusable modules: regions, address-plan, network-aws, network-azure,
                      network-gcp, interconnect (Postgres and etcd to come)
  envs/<name>/        root configuration per environment, with deployment.json naming its geo
```

## Rules

- **Agents never run `tofu apply`**, or anything that changes a real cloud account. CI only
  formats, validates and lints. It holds no cloud credentials. A person applies, following [`docs/infrastructure.md`](../docs/infrastructure.md), by hand or through a pipeline of their own that they approve.
- **Regions by geo.** `infra/geos.json` lists each geo's regions. Every region of an enabled
  geo is allowed. Each environment names its geo in `envs/<name>/deployment.json`
  (`{"geo": "uk"}`), and `scripts/check_uk_regions.py` fails CI when any `.tf` or `.tfvars`
  file uses a region outside that geo. Modules take their regions from `modules/regions`.
  To add Europe, the US or Asia, a person adds the geo to `geos.json` and enables it. The
  guard does not change. Multi-regions that span geos (Google `EU`, `US`, `ASIA`) are never
  allowed.
- **No state, no secrets.** State, `.terraform/` and `*.tfstate*` are ignored by git. Outputs
  carry hosts, ports and secret references, never passwords.
- **Pinned providers.** Every module that uses `aws`, `azurerm` or `google` declares the
  version in `providers.json`.

## Networks

`modules/address-plan` turns `address_plan.json` into one range per cloud for a geo, and splits
each range the same way on all three clouds: three tiers (`cell`, `control_plane`, `api`), each
with three subnets, one per availability zone. The three ranges never overlap, so private links
can route between them. A test checks the plan with Python's `ipaddress`.

`modules/network-aws`, `network-azure` and `network-gcp` each build one network from that plan:

- **Private only.** No internet gateway or NAT, no public addresses, no default route out. Later
  modules add the routes and rules they need.
- **Default deny.** AWS security groups hold no rules. Azure network security groups deny
  internet in and out and everything else in and out (priority 4096, so allow rules go below it). Google
  denies all ingress unless a firewall rule allows it.
- **Flow logs on.** CloudWatch Logs on AWS, a virtual-network flow log to a zone-redundant,
  private storage account on Azure, and log config on every Google subnet.
- **Zones.** AWS subnets are per zone. Azure and Google subnets span zones, so subnet number N of
  a tier is where zone N resources go (Azure zones 1 to 3, Google zones a to c). Addressing then
  matches on all three clouds.

## Links between the clouds

`modules/interconnect` joins the three networks and decides what may cross. It takes the three
network modules' outputs, the address plan, and the tunnels' pre-shared keys.

- **Mechanism.** The `mechanism` setting defaults to `ipsec_vpn`: site-to-site IPsec VPN with BGP,
  so each side learns the others' ranges and drops them when a tunnel fails. It is the only value
  built today. A private interconnect would be a further value.
- **Tunnels.** AWS and Azure 2, AWS and Google 4, Azure and Google 2, so every pair has at least
  two. IKEv2 with AES-256, SHA-256 and group 14. `links.json` holds the BGP numbers and the
  link-local /30 inside each tunnel.
- **Public addresses.** Only the VPN gateways have one (Azure's, and Google's two interfaces).
  They accept IPsec from the peer gateways and nothing else. No workload subnet has a public
  address or an internet route.
- **Keys.** The `preshared_keys` input is sensitive and has no default. A person supplies 32 to
  64 letters and digits per tunnel from a secret store when applying. The names are in the
  `required_preshared_key_names` output. Keys end up in state, so state must be private.
- **Flows.** `flows.json` lists the only allowed traffic between tiers: API and cells to Postgres,
  cells to cells on the relay port, API and cells to the control plane's etcd client port, and
  control plane members to each other on the peer port. Each rule is built from that file and
  everything else stays denied. Google cannot limit an egress rule by source subnet, so its
  limit on the sending side is by destination and port; the receiving side limits by source.
- **The relay port** (`8443`) is a proposed default. The relay is built in R-01, which should
  confirm or change it in `flows.json`.

## Postgres

`modules/postgres` gives a cell PostgreSQL 16 in one of three forms behind one interface. Every form is vanilla: no extensions and no logical replication slots, so the kernel cannot tell them apart. Outputs are `host`, `port`, `secret_ref` and `kind`. There is never a password or a connection string.

| Form | Where | Standbys | Sign-in |
| --- | --- | --- | --- |
| `lakebase` (default where `databricks` is given) | Databricks Lakebase | two, in other zones, one readable | OAuth service principal |
| `managed` | RDS Multi-AZ cluster, Azure zone-redundant flexible server, Cloud SQL regional | one or two, in other zones | RDS-managed secret, Entra, IAM |
| `self_hosted` | three machines, one per zone | two, `ANY 1` synchronous | credential written by the primary's first start |

Things to know:

- **Lakebase cannot be proven private or keyed here.** Private Link and the customer-managed key belong to the Databricks workspace, not the database. The caller sets `private_link` and `customer_managed_key` to true to say the tenant's workspace has both. The module cannot check this, and refuses without it. Whether Lakebase commits synchronously is not stated in its documentation.
- **Self-hosted runs the node image as a container.** Each machine boots Flatcar Container Linux, a minimal operating system that holds docker and little else, from an Ignition file made by `modules/node-boot` (one for all three clouds). The `image` input is the node image from `images/postgres`, in your private registry, **by digest**: `registry.example/repo/name@sha256:<64 hex digits>`. A tag is refused, because a tag can change under a running cell. The machine signs in to the registry with its own cloud identity, so no credential is stored, pulls the image, and runs it read-only with no added privileges. The operating system's own updates are off: they need the internet, and a node image is updated by replacing machines one at a time. `operating_system_image_id` (Google: `operating_system_image`) is the Flatcar image for the region. It is not checked until I-01f applies it.
- **Self-hosted nodes have no internet.** They have no public address and no route out. They reach the registry and the cloud's secret store only over private endpoints, made by `modules/private-access-aws`, `-azure` and `-gcp`: AWS interface endpoints for the registry and Secrets Manager plus a gateway endpoint for image layers; Azure private endpoints and private DNS for the registry (Premium tier) and the Key Vault; Google Private Service Connect for Google's APIs with private DNS. The registry itself is yours to make, in the same tenant.
- **Self-hosted nodes reach one secret.** The node configuration names the cloud's secret store (`secret_store`) and the secret, and the machine's own identity may read and write that secret and nothing else: an IAM policy on the one secret on AWS; a custom role with two data actions, assigned on the one secret, on Azure (the module makes the secret with a placeholder value that it never overwrites); and the version-adder and accessor roles on the one secret on Google. Each may also read the node image from the registry.
- **Self-hosted nodes read their certificates from the secret store.** Each node has a secret for its certificates and keys, in the cloud's secret store, and an identity of its own that reads that secret and no other. A script on the machine reads it before the container starts and writes `/etc/ledger/tls`. OpenTofu makes the secret and never its contents, so no certificate or key is in the state. Whoever issues the certificates puts the values in. See `docs/self-hosted-postgres.md`.
- **Self-hosted has no automatic failover.** Promoting a standby is manual. It is a follow-up task.
- **Azure managed** needs the private DNS zone from `modules/postgres/azure-dns`, made once for each network.
- Nodes take fixed addresses (host number 10) in their zone's subnet. The host output is the first node's address.

## Control plane

`modules/control-plane` runs the etcd v3 cluster that decides which cell is home for each account (decision D-03). It is shared by every tenant, so it has its own network and not a tenant's.

- **Five members over the three clouds by default, placed two, two and one** (AWS two, Azure two, Google one), so that losing any one cloud leaves a majority of three. `members` can be lower, but never below three: fewer is refused by input validation. Four is refused too, because losing the cloud with two of them would leave two of four. Where a geography has one region, give `clouds = ["aws"]` (or one other) and the three zones of that cloud are the failure domains. Two clouds are refused.
- **The placement is plain arithmetic** in `control-plane/placement`, and `tofu test` checks it in CI: three and five members, the refusals, and that no two members share an address. Run it with `tofu -chdir=infra/modules/control-plane/placement test` after `tofu init -backend=false`.
- **Members are machines on Flatcar Container Linux** (`modules/etcd-boot`, the same boot as the Postgres nodes), running etcd as a container from an image given by digest from a private registry, with no public address. They reach the registry and the secret store over the private endpoints from `modules/private-access-*`. Members have fixed addresses (host 10 and up in their zone's range), so the initial cluster is known before any machine exists. Only ports 2379 and 2380 are open between control plane tiers, from `flows.json`.
- **Mutual TLS, TLS 1.3 at least.** Clients and peers must both present a certificate from one authority. Each member has a secret in its cloud's secret store, holding `ca.crt`, `server.crt`, `server.key`, `peer.crt` and `peer.key` as one JSON object, and an identity of its own that reads only that secret. OpenTofu makes the secrets and never their contents, so no certificate is in the repository or the state. Whoever runs the authority issues them: the server and peer certificates must name the member's address, and clients (the API, cells and the relay) need a client certificate. The `certificate_secrets` output names each secret.
- **The module has no way to add a member to a running cluster, or to replace one.** Changing `members` on a live cluster is a runbook step, not an apply. Nothing has been applied (I-01f).
- The inputs per cloud are the control plane tier's subnets and group from `modules/network-*`, plus the registry, machine image and key details the Postgres modules also take.

## Checks, locally and in CI

```bash
python scripts/check_uk_regions.py        # region guard
tofu fmt -check -recursive infra
for d in $(find infra -name '*.tf' -printf '%h\n' | sort -u); do
  tofu -chdir="$d" init -backend=false && tofu -chdir="$d" validate
done
tflint --init --config "$PWD/infra/.tflint.hcl"
tflint --chdir infra --recursive --config "$PWD/infra/.tflint.hcl"
```

The `infra` workflow runs the same on every push and pull request.
