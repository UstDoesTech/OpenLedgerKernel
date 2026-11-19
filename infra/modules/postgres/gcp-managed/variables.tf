variable "name" {
  description = "The cell's name. It names the instance and its parts."
  type        = string
}

variable "tier" {
  description = "The machine tier, for example db-custom-2-7680."
  type        = string
}

variable "storage_gb" {
  description = "Disk size in gigabytes. It grows automatically."
  type        = number
}

variable "region" {
  description = "The Google Cloud region. It comes from modules/regions."
  type        = string
}

variable "network_id" {
  description = "The VPC network from modules/network-gcp."
  type        = string
}

variable "subnet_id" {
  description = "The cell tier subnet that holds the private endpoint's address."
  type        = string
}

variable "kms_key_id" {
  description = "The tenant's Cloud KMS key, in the instance's region. Cloud SQL's service agent must be allowed to use it."
  type        = string
}

variable "administrator_email" {
  description = "The service account that administers the instance. Password sign-in is not set up, so this is the only way in."
  type        = string
}

variable "backup_retention_days" {
  description = "How long backups are kept, in days."
  type        = number
}

variable "deletion_protection" {
  description = "Refuse to delete the instance."
  type        = bool
}

variable "labels" {
  description = "Labels for every resource that takes them."
  type        = map(string)
  default     = {}
}
