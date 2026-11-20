output "tenant" {
  description = "The tenant these machines belong to."
  value       = local.tenant
}

output "apis" {
  description = "Each API machine: the cell it runs next to, its private address and port, and where its secret is. Never a secret's contents. Whoever fills the secrets needs secret_ref and secret_files."
  value = {
    "aws" = {
      cell           = local.names.aws
      address        = module.api_aws.address
      port           = local.client_port
      internal_port  = local.internal_port
      secret_ref     = module.api_aws.secret_ref
      secret_files   = module.api_aws.secret_files
      tls_terminates = module.api_aws.tls_terminates
    }
    "azure" = {
      cell           = local.names.azure
      address        = module.api_azure.address
      port           = local.client_port
      internal_port  = local.internal_port
      secret_ref     = module.api_azure.secret_ref
      secret_files   = module.api_azure.secret_files
      tls_terminates = module.api_azure.tls_terminates
    }
    "gcp" = {
      cell           = local.names.gcp
      address        = module.api_gcp.address
      port           = local.client_port
      internal_port  = local.internal_port
      secret_ref     = module.api_gcp.secret_ref
      secret_files   = module.api_gcp.secret_files
      tls_terminates = module.api_gcp.tls_terminates
    }
  }
}
