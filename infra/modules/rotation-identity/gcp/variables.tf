variable "name" {
  description = "The service account's name."
  type        = string
}

variable "environment" {
  description = "The GitHub environment whose token may act as the service account, and no other."
  type        = string
}

variable "workload_identity_pool" {
  description = "The workload identity pool the infrastructure identities already use. Its provider's attribute condition must list this environment (docs/credentialed-workflows.md)."
  type        = string
}

variable "api_secret_id" {
  description = "The Google Cloud cell's API secret, as projects/<project>/secrets/<name>. The service account's roles are on this secret only."
  type        = string
}

variable "administrator_secret_id" {
  description = "The Google Cloud cell's administrator credential secret, for a proving run. Empty otherwise."
  type        = string
  default     = ""
}

variable "node_secret_ids" {
  description = "The secrets that hold the certificates of the Google Cloud cell's database machines, as projects/<project>/secrets/<name>. The service account may add a version to each, and no more."
  type        = list(string)
  default     = []
}
