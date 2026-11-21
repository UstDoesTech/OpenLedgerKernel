variable "name" {
  description = "The identity's name."
  type        = string
}

variable "location" {
  description = "The region the identity is kept in, from modules/regions."
  type        = string
}

variable "environment" {
  description = "The GitHub environment whose token the identity trusts, and no other."
  type        = string
}

variable "subject_prefix" {
  description = "The start of the token's subject: repo:<owner>/<repository>, or the immutable form the repository sends."
  type        = string
}

variable "vault_id" {
  description = "The Key Vault that holds the Azure cell's API secret. The identity's roles are on this vault only."
  type        = string
}

variable "administrator_vault_id" {
  description = "The Key Vault that holds the Azure cell's administrator credential, if it is not the vault above, for a proving run. Empty otherwise."
  type        = string
  default     = ""
}
