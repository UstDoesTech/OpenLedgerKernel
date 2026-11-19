variable "azure_subscription_id" {
  description = "The Azure subscription that holds the platform. It names an account, it is not a credential. Supply it as the TF_VAR_azure_subscription_id secret."
  type        = string
}

variable "state_bucket" {
  description = "The bucket that holds the state, to read the networks layer's outputs. It names a bucket, it is not a credential. Supply it as the TF_VAR_state_bucket secret."
  type        = string
}

variable "preshared_keys" {
  description = "The IPsec pre-shared key of every tunnel, by name, as a JSON map. The names are in the required_preshared_key_names output of the interconnect module (aws-azure-0, aws-azure-1, aws-gcp-0 to aws-gcp-3, azure-gcp-0 and azure-gcp-1). Each key is 32 to 64 letters and digits, and no two are the same. Supply it as the TF_VAR_preshared_keys secret, never from a file."
  type        = map(string)
  sensitive   = true
}
