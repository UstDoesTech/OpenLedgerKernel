# One address plan for every network, so private links can route between clouds. The source
# of truth is ../../address_plan.json. Each cloud gets one /16 per geo. Inside it, every tier
# (cell, control plane, API) gets one subnet per availability zone, carved the same way on all
# three clouds: subnet number is tier index times tier_stride plus zone index. Each network
# also gets one small gateway subnet, the last /27, which sits in a slot no tier uses.
#
# AWS has one more tier, edge, for the load balancer in front of the API (D-22). It is numbered
# after the other tiers, so no address that exists moves, and Azure and Google Cloud have none.
locals {
  plan   = jsondecode(file("${path.module}/../../address_plan.json"))
  layout = local.plan.layout
  cidrs  = local.plan.geos[var.geo]

  # The tiers of each cloud, in numbering order: the shared ones, then any that only one cloud has.
  tiers = {
    for cloud in keys(local.cidrs) : cloud => concat(
      local.layout.tiers,
      cloud == "aws" ? try(local.layout.aws_only_tiers, []) : [],
    )
  }

  subnets = {
    for cloud, cidr in local.cidrs : cloud => {
      for tier_index, tier in local.tiers[cloud] : tier => [
        for zone in range(local.layout.zones) : cidrsubnet(cidr, local.layout.subnet_newbits, tier_index * local.layout.tier_stride + zone)
      ]
    }
  }

  gateway_subnets = {
    for cloud, cidr in local.cidrs : cloud => cidrsubnet(cidr, local.layout.gateway_newbits, local.layout.gateway_number)
  }
}
