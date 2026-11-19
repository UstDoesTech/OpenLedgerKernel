variable "azure_subscription_id" {
  description = "The Azure subscription that holds the platform. It names an account, it is not a credential. Supply it as the TF_VAR_azure_subscription_id secret."
  type        = string
}

variable "state_bucket" {
  description = "The bucket that holds the state, to read the networks layer's outputs. It names a bucket, it is not a credential. Supply it as the TF_VAR_state_bucket secret."
  type        = string
}
