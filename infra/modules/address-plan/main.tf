# One address plan for every network, so private links can route between clouds. The source
# of truth is ../../address_plan.json. Each cloud gets one /16 per geo. Inside it, every tier
# (cell, control plane, API) gets one subnet per availability zone, carved the same way on all
# three clouds: subnet number is tier index times tier_stride plus zone index. Each network
# also gets one small gateway subnet, the last /27, which sits in a slot no tier uses.
locals {
  plan   = jsondecode(file("${path.module}/../../address_plan.json"))
  layout = local.plan.layout
  cidrs  = local.plan.geos[var.geo]

  subnets = {
    for cloud, cidr in local.cidrs : cloud => {
      for tier_index, tier in local.layout.tiers : tier => [
        for zone in range(local.layout.zones) : cidrsubnet(cidr, local.layout.subnet_newbits, tier_index * local.layout.tier_stride + zone)
      ]
    }
  }

  gateway_subnets = {
    for cloud, cidr in local.cidrs : cloud => cidrsubnet(cidr, local.layout.gateway_newbits, local.layout.gateway_number)
  }
}
