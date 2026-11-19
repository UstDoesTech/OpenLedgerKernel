# Encrypted private links between the three networks, and the rules that decide what may cross
# them. Every pair of networks is joined by IPsec tunnels carrying BGP, so each side learns the
# other's ranges and loses them when a tunnel drops. Policy comes from two files:
#   ../../links.json  BGP numbers and the link-local range inside each tunnel
#   ../../flows.json  the only traffic allowed between tiers; everything else is denied
#
# Tunnels per pair: AWS and Azure 2, AWS and Google 4, Azure and Google 2. The VPN endpoints
# are the only public addresses in the platform. They accept IPsec from the peer gateways and
# nothing else, and no workload subnet has a public address or an internet route.
locals {
  links     = jsondecode(file("${path.module}/../../links.json"))
  flow_file = jsondecode(file("${path.module}/../../flows.json"))

  asn     = local.links.asn
  tunnels = local.links.tunnels
  tags    = merge(var.tags, { managed_by = "opentofu", mechanism = var.mechanism })

  # IPsec settings for every tunnel: IKEv2, AES-256, SHA-256 and Diffie-Hellman group 14, with
  # rekeying inside the limits AWS accepts. Azure takes the same suite in its own spelling.
  # Google negotiates IKEv2 from a set that includes it.
  ipsec = {
    ike_versions    = ["ikev2"]
    dh_groups       = [14]
    encryption      = ["AES256"]
    integrity       = ["SHA2-256"]
    phase1_lifetime = 28800
    phase2_lifetime = 3600
  }

  preshared_key_names = flatten([for pair, cidrs in local.tunnels : [for index, cidr in cidrs : "${pair}-${index}"]])

  tiers = keys(var.plans.aws.subnets)

  # Every subnet of a tier on all three clouds.
  tier_cidrs = {
    for tier in local.tiers : tier => flatten([for plan in values(var.plans) : plan.subnets[tier]])
  }

  # The allowed flows, numbered so each gets its own rule priority.
  flows = {
    for index, flow in local.flow_file.flows : flow.name => merge(flow, { index = index, port = local.flow_file.ports[flow.port] })
  }
}

resource "terraform_data" "preshared_key_check" {
  lifecycle {
    precondition {
      condition     = alltrue([for name in local.preshared_key_names : contains(keys(var.preshared_keys), name)]) && length(distinct(values(var.preshared_keys))) == length(var.preshared_keys)
      error_message = "preshared_keys must hold a key for every name in required_preshared_key_names, and no two keys may be the same."
    }
  }
}
