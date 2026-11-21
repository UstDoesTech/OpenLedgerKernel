variable "azure_subscription_id" {
  description = "The Azure subscription. An account identifier, not a credential. Supply it as the TF_VAR_azure_subscription_id secret."
  type        = string
}

variable "state_bucket" {
  description = "The bucket that holds the state, read to find the layers' outputs, and the one the rotation identity may read. It names a bucket, it is not a credential. Supply it as the TF_VAR_state_bucket secret."
  type        = string
}

variable "plan_store" {
  description = "Where the runs keep their logs, as s3://<bucket>/<prefix>. The AWS role may write there and nowhere else. It names a bucket, it is not a credential. Supply it as the TF_VAR_plan_store secret."
  type        = string

  validation {
    condition     = can(regex("^s3://[a-z0-9.-]+(/[A-Za-z0-9._/-]*)?$", var.plan_store))
    error_message = "The plan store is an s3:// address."
  }
}

variable "workload_identity_pool" {
  description = "The Google Cloud workload identity pool the infrastructure identities already use. Supply it as the TF_VAR_workload_identity_pool secret."
  type        = string
}

variable "proving" {
  description = "True for a proving environment, which holds no real money or customer data (decision D-19). The identities may then read each cell's administrator credential, so a run can fill a missing database connection. Supply it as the TF_VAR_proving secret."
  type        = bool
  default     = false
}

variable "oidc_subject_prefix" {
  description = "The start of the GitHub token's subject, if the repository sends the immutable form instead of repo:<owner>/<repository>. Empty for the plain form."
  type        = string
  default     = ""
}

variable "encryption_arn" {
  description = "The AWS key that encrypts the API secret, if it is not the account's default key. Empty otherwise."
  type        = string
  default     = ""
}
