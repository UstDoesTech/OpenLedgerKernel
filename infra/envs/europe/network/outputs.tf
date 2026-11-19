output "aws" {
  description = "The AWS network, for the layers that read this one."
  value = {
    vpc_id             = module.network_aws.vpc_id
    vpc_cidr           = module.network_aws.vpc_cidr
    subnet_ids         = module.network_aws.subnet_ids
    route_table_id     = module.network_aws.route_table_id
    security_group_ids = module.network_aws.security_group_ids
  }
}

output "azure" {
  description = "The Azure network, for the layers that read this one."
  value = {
    resource_group_name          = module.network_azure.resource_group_name
    network_id                   = module.network_azure.network_id
    network_name                 = module.network_azure.network_name
    network_cidr                 = module.network_azure.network_cidr
    subnet_ids                   = module.network_azure.subnet_ids
    network_security_group_ids   = module.network_azure.network_security_group_ids
    network_security_group_names = module.network_azure.network_security_group_names
  }
}

output "gcp" {
  description = "The Google Cloud network, for the layers that read this one."
  value = {
    network_id   = module.network_gcp.network_id
    network_name = module.network_gcp.network_name
    network_cidr = module.network_gcp.network_cidr
    subnet_ids   = module.network_gcp.subnet_ids
  }
}

output "address_plan" {
  description = "The address plan, including the gateway subnets the interconnect layer needs."
  value       = module.address_plan
}
