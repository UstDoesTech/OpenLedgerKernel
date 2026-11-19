variable "name" {
  description = "The cell's name. It names the machines and the secret."
  type        = string
}

variable "tenant" {
  description = "The tenant's id. It goes in the node configuration."
  type        = string
}

variable "instance_type" {
  description = "The machine type for all three nodes."
  type        = string
}

variable "storage_gb" {
  description = "Data volume size in gigabytes."
  type        = number
}

variable "image" {
  description = "The node image in the private registry, by digest. A tag is refused."
  type        = string

  validation {
    condition     = can(regex("@sha256:[0-9a-f]{64}$", var.image))
    error_message = "The image must be given by digest, like registry.example/repo/name@sha256:<64 hex digits>. Tags are refused."
  }
}

variable "operating_system_image_id" {
  description = "The Flatcar Container Linux machine image for the region. It is the operating system only: the node runs as a container on it."
  type        = string
}

variable "registry_repository_arn" {
  description = "The private registry repository that holds the node image. The nodes may read it."
  type        = string
}

variable "vpc_id" {
  description = "The VPC the nodes are in."
  type        = string
}

variable "region" {
  description = "The AWS region. It comes from modules/regions."
  type        = string
}

variable "route_table_id" {
  description = "The private route table. The gateway endpoint for object storage adds a route here."
  type        = string
}

variable "subnet_ids" {
  description = "The three subnets, one per availability zone."
  type        = list(string)

  validation {
    condition     = length(var.subnet_ids) == 3
    error_message = "Give exactly three subnets, one per availability zone."
  }
}

variable "subnet_cidrs" {
  description = "The ranges of those subnets, in the same order. Each node takes a fixed address from its range."
  type        = list(string)

  validation {
    condition     = length(var.subnet_cidrs) == 3
    error_message = "Give exactly three ranges, one per subnet."
  }
}

variable "security_group_ids" {
  description = "Security groups for the nodes. Use the cell tier's group."
  type        = list(string)
}

variable "kms_key_arn" {
  description = "The tenant's KMS key. It encrypts the volumes and the secret."
  type        = string
}

variable "tags" {
  description = "Tags for every resource."
  type        = map(string)
  default     = {}
}

variable "reuse_private_access" {
  description = "Use the private endpoints and DNS zones that already exist in this network (the environment's private-access layer) and make none. Leave false in a network that has none, such as a customer's own."
  type        = bool
  default     = false
}
