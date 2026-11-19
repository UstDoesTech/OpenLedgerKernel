output "aws" {
  description = "AWS regions in the chosen geo."
  value       = local.regions.aws

  precondition {
    condition     = local.geo_ok
    error_message = "The geo is not enabled in infra/geos.json."
  }
}

output "azure" {
  description = "Azure regions in the chosen geo."
  value       = local.regions.azure

  precondition {
    condition     = local.geo_ok
    error_message = "The geo is not enabled in infra/geos.json."
  }
}

output "gcp" {
  description = "Google Cloud regions in the chosen geo."
  value       = local.regions.gcp

  precondition {
    condition     = local.geo_ok
    error_message = "The geo is not enabled in infra/geos.json."
  }
}
