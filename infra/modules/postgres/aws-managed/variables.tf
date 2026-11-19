variable "name" {
  description = "The cell's name. It names the cluster and its parts."
  type        = string
}

variable "instance_class" {
  description = "The DB instance class. It must be one that Multi-AZ DB clusters support, such as db.m6gd.large."
  type        = string
}

variable "storage_gb" {
  description = "Allocated storage in gigabytes. Multi-AZ DB clusters need at least 100."
  type        = number
}

variable "iops" {
  description = "Provisioned IOPS for the cluster's storage."
  type        = number
  default     = 1000
}

variable "subnet_ids" {
  description = "The three subnets the cluster uses, one per availability zone."
  type        = list(string)

  validation {
    condition     = length(var.subnet_ids) == 3
    error_message = "Give exactly three subnets, one per availability zone."
  }
}

variable "security_group_ids" {
  description = "Security groups for the cluster. Use the cell tier's group, whose rules come from modules/interconnect."
  type        = list(string)
}

variable "kms_key_arn" {
  description = "The tenant's KMS key. It encrypts the storage, the administrator secret and the logs."
  type        = string
}

variable "backup_retention_days" {
  description = "How long backups are kept, in days."
  type        = number
}

variable "deletion_protection" {
  description = "Refuse to delete the cluster."
  type        = bool
}

variable "tags" {
  description = "Tags for every resource."
  type        = map(string)
  default     = {}
}
