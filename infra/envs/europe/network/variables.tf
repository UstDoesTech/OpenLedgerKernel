variable "azure_subscription_id" {
  description = "The Azure subscription that holds the platform. It names an account, it is not a credential: the run signs in through the federated identity. Supply it as the TF_VAR_azure_subscription_id secret."
  type        = string
}
