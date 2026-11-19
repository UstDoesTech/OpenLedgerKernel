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

variable "cell_kind" {
  description = "Who runs each cell's Postgres: \"self_hosted\" (three machines running the node image, the default) or \"managed\" (RDS Multi-AZ cluster, Azure zone-redundant flexible server, Cloud SQL regional instance). Changing it replaces the cells: the old kind is destroyed and the new one made, with no data carried across. Not a secret."
  type        = string
  default     = "self_hosted"

  validation {
    condition     = contains(["self_hosted", "managed"], var.cell_kind)
    error_message = "The cell kind must be self_hosted or managed."
  }
}

variable "deletion_protection" {
  description = "Refuse to delete a managed cell's database. Leave true for any cell that holds data. Set to false, and apply, before destroying a proving environment. Not used by self-hosted cells."
  type        = bool
  default     = true
}
