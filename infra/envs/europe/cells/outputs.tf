output "tenant" {
  description = "The tenant these cells belong to."
  value       = local.tenant
}

output "cells" {
  description = "Each cell's kind, private host, port and where its administrator's credential is held. Never a credential."
  value = {
    "aws" = {
      kind       = module.cell_aws.kind
      host       = module.cell_aws.host
      port       = module.cell_aws.port
      secret_ref = module.cell_aws.secret_ref
    }
    "azure" = {
      kind       = module.cell_azure.kind
      host       = module.cell_azure.host
      port       = module.cell_azure.port
      secret_ref = module.cell_azure.secret_ref
    }
    "gcp" = {
      kind       = module.cell_gcp.kind
      host       = module.cell_gcp.host
      port       = module.cell_gcp.port
      secret_ref = module.cell_gcp.secret_ref
    }
  }
}
