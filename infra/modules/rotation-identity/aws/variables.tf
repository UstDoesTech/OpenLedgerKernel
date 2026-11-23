variable "name" {
  description = "The role's name."
  type        = string
}

variable "environment" {
  description = "The GitHub environment whose token the role trusts, and no other."
  type        = string
}

variable "subject_prefix" {
  description = "The start of the token's subject: repo:<owner>/<repository>, or the immutable form the repository sends."
  type        = string
}

variable "authority_key_arn" {
  description = "The KMS key that signs. The role may sign with it and read its public half."
  type        = string
}

variable "authority_certificate_arn" {
  description = "The secret that holds the authority's public certificate."
  type        = string
}

variable "api_secret_arn" {
  description = "The AWS cell's API secret."
  type        = string
}

variable "administrator_secret_arn" {
  description = "The AWS cell's administrator credential, which a proving run reads to fill a database connection. Empty for an environment that is not a proving one."
  type        = string
  default     = ""
}

variable "encryption_arn" {
  description = "The key that encrypts the API secret, if it is not the account's default key. Empty otherwise."
  type        = string
  default     = ""
}

variable "state_bucket" {
  description = "The bucket that holds the state, read to find where the cells and the authority are."
  type        = string
}

variable "state_keys" {
  description = "The state objects the role may read: one for each layer it reads."
  type        = list(string)
}

variable "plan_store_bucket" {
  description = "The bucket that keeps the runs' logs."
  type        = string
}

variable "plan_store_prefix" {
  description = "The prefix in that bucket the role may write under. Empty for the whole bucket."
  type        = string
}

variable "state_encryption_arn" {
  description = "The key that encrypts the state bucket, if it is not the bucket's default key. The role may decrypt with it, to read the state. Empty otherwise."
  type        = string
  default     = ""
}

variable "plan_store_encryption_arn" {
  description = "The key that encrypts the plan store, if it is not the bucket's default key. The role may generate data keys with it, to write logs. Empty otherwise."
  type        = string
  default     = ""
}
