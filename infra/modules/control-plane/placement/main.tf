# Where the etcd members go. This is pure arithmetic and makes nothing, so it can be tested
# without a cloud account (see tests/).
#
# Members are dealt round the failure domains in turn, so five members over three clouds are
# placed two, two and one, and three are placed one each. Within a cloud they are dealt round the
# availability zones. With one cloud, the failure domains are its zones.
locals {
  single_cloud = length(var.clouds) == 1
  domains      = local.single_cloud ? ["zone-1", "zone-2", "zone-3"] : var.clouds
  quorum       = floor(var.members / 2) + 1

  members = [for i in range(var.members) : {
    index  = i
    name   = "m${i + 1}"
    domain = local.domains[i % 3]
    cloud  = local.single_cloud ? var.clouds[0] : local.domains[i % 3]
    zone   = local.single_cloud ? i % 3 : floor(i / 3) % 3
    # A slot within the zone, so two members in one zone get different addresses.
    slot = local.single_cloud ? floor(i / 3) : floor(i / 9)
  }]

  per_domain = { for d in local.domains : d => length([for m in local.members : m if m.domain == d]) }
  per_cloud  = { for c in ["aws", "azure", "gcp"] : c => length([for m in local.members : m if m.cloud == c]) }

  # What is left of the cluster if any one failure domain is lost.
  survivors_after_loss = { for d, n in local.per_domain : d => var.members - n }
}

# Each member with its full name and fixed address: host 10 and up in its zone's subnet. The
# initial cluster is the same for every member, and is known before any machine exists.
locals {
  placed = [for m in local.members : merge(m, {
    full_name = "${var.name}-${m.name}"
    address   = try(cidrhost(var.cidrs[m.cloud][m.zone], 10 + m.slot), null)
  })]

  initial_cluster = join(",", [for m in local.placed : "${m.full_name}=https://${coalesce(m.address, "unassigned")}:2380"])
}
