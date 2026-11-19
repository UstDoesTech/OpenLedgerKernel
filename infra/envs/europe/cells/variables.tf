variable "azure_subscription_id" {
  description = "The Azure subscription that holds the platform. It names an account, it is not a credential. Supply it as the TF_VAR_azure_subscription_id secret."
  type        = string
}

variable "state_bucket" {
  description = "The bucket that holds the state, to read the earlier layers. It names a bucket, it is not a credential. Supply it as the TF_VAR_state_bucket secret."
  type        = string
}

variable "azure_flatcar_image_id" {
  description = "The Flatcar Container Linux image in Azure, as a resource id of a managed or gallery image in the deploying subscription and the geo's region. Supply it as the TF_VAR_azure_flatcar_image_id secret."
  type        = string
}

variable "administrator_ssh_public_key" {
  description = "The SSH public key for the Azure nodes' administrator account. A public key, but marked sensitive so a plan does not print it. Supply it as the TF_VAR_administrator_ssh_public_key secret."
  type        = string
  sensitive   = true
}
