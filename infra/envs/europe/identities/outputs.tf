output "environment_secrets" {
  description = "The values a person enters, as secrets of the rotate-credentials GitHub environment. Identifiers, not credentials. AZURE_SUBSCRIPTION_ID and the Google Cloud provider's secret are the ones the infra-apply environment already holds."
  value = {
    AWS_ROLE_ARN        = module.aws.role_arn
    AZURE_CLIENT_ID     = module.azure.client_id
    AZURE_TENANT_ID     = module.azure.tenant_id
    GCP_SERVICE_ACCOUNT = module.gcp.service_account
  }
}

output "aws_role_arn" {
  description = "The AWS role."
  value       = module.aws.role_arn
}

output "aws_boundary_arn" {
  description = "The permissions boundary on the AWS role."
  value       = module.aws.boundary_arn
}

output "azure_client_id" {
  description = "The Azure identity's client id."
  value       = module.azure.client_id
}

output "gcp_service_account" {
  description = "The Google Cloud service account."
  value       = module.gcp.service_account
}
