variable "azure_subscription_id" {
  description = "The Azure subscription that holds the platform. It names an account, it is not a credential. Supply it as the TF_VAR_azure_subscription_id secret."
  type        = string
}

variable "state_bucket" {
  description = "The bucket that holds the state, to read the earlier layers. It names a bucket, it is not a credential. Supply it as the TF_VAR_state_bucket secret."
  type        = string
}

variable "postgres_image_digest" {
  description = "The digest of the PostgreSQL node image, for example sha256:<64 hex digits>. One digest, because the image is copied between registries and keeps it. Supply it as the TF_VAR_postgres_image_digest secret."
  type        = string

  validation {
    condition     = can(regex("^sha256:[0-9a-f]{64}$", var.postgres_image_digest))
    error_message = "Give a digest of the form sha256: and 64 lower-case hex digits. Tags are refused."
  }
}

variable "azure_administrator_object_id" {
  description = "The object id of the Entra group that administers this tenant's PostgreSQL servers. A person creates the group (see docs/infrastructure.md). It names a group, it is not a credential. Supply it as the TF_VAR_azure_administrator_object_id secret."
  type        = string

  validation {
    condition     = can(regex("^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$", var.azure_administrator_object_id))
    error_message = "Give the group's object id, a UUID."
  }
}

variable "azure_administrator_name" {
  description = "The display name of that Entra group. Supply it as the TF_VAR_azure_administrator_name secret."
  type        = string
}
