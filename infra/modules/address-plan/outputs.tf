output "aws" {
  description = "The AWS network's CIDR and its subnets: a list of three per tier, one per availability zone, and a gateway subnet."
  value = {
    cidr           = local.cidrs.aws
    subnets        = local.subnets.aws
    gateway_subnet = local.gateway_subnets.aws
  }
}

output "azure" {
  description = "The Azure network's CIDR and its subnets: a list of three per tier, one per availability zone, and a gateway subnet."
  value = {
    cidr           = local.cidrs.azure
    subnets        = local.subnets.azure
    gateway_subnet = local.gateway_subnets.azure
  }
}

output "gcp" {
  description = "The Google Cloud network's CIDR and its subnets: a list of three per tier, one per availability zone, and a gateway subnet."
  value = {
    cidr           = local.cidrs.gcp
    subnets        = local.subnets.gcp
    gateway_subnet = local.gateway_subnets.gcp
  }
}
