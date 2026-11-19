variable "state_bucket" {
  description = "The bucket that holds the state, to read the private access layer. It names a bucket, it is not a credential. Supply it as the TF_VAR_state_bucket secret."
  type        = string
}
