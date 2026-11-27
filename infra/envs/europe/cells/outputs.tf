output "tenant" {
  description = "The tenant these cells belong to."
  value       = local.tenant
}

output "cells" {
  description = "Each cell's kind, private host, port and where its administrator's credential is held, and for a self-hosted cell its nodes and where their certificates go. Never a credential or a certificate."
  value = {
    "aws" = {
      kind       = module.cell_aws.kind
      host       = module.cell_aws.host
      port       = module.cell_aws.port
      secret_ref = module.cell_aws.secret_ref
      nodes      = module.cell_aws.nodes
      # Only a managed cell has one. The API layer names it in the machine's sign-in grant.
      database_resource_id = module.cell_aws.database_resource_id
    }
    "azure" = {
      kind       = module.cell_azure.kind
      host       = module.cell_azure.host
      port       = module.cell_azure.port
      secret_ref = module.cell_azure.secret_ref
      nodes      = module.cell_azure.nodes
    }
    "gcp" = {
      kind       = module.cell_gcp.kind
      host       = module.cell_gcp.host
      port       = module.cell_gcp.port
      secret_ref = module.cell_gcp.secret_ref
      nodes      = module.cell_gcp.nodes
    }
  }
}
