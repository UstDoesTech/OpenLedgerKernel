variable "name" {
  description = "The cell's name. It names the machines and the secret."
  type        = string
}

variable "tenant" {
  description = "The tenant's id. It goes in the node configuration."
  type        = string
}

variable "machine_type" {
  description = "The machine type for all three nodes."
  type        = string
}

variable "storage_gb" {
  description = "Data disk size in gigabytes."
  type        = number
}

variable "region" {
  description = "The Google Cloud region. It comes from modules/regions."
  type        = string
}

variable "image" {
  description = "The node image in the private registry, by digest. A tag is refused."
  type        = string

  validation {
    condition     = can(regex("@sha256:[0-9a-f]{64}$", var.image))
    error_message = "The image must be given by digest, like registry.example/repo/name@sha256:<64 hex digits>. Tags are refused."
  }
}

variable "operating_system_image" {
  description = "The Flatcar Container Linux image. It is the operating system only: the node runs as a container on it."
  type        = string
}

variable "registry_repository_id" {
  description = "The private Artifact Registry repository that holds the node image, as projects/<project>/locations/<location>/repositories/<name>. The nodes may read it."
  type        = string

  validation {
    condition     = can(regex("^projects/[^/]+/locations/[^/]+/repositories/[^/]+$", var.registry_repository_id))
    error_message = "Give the repository as projects/<project>/locations/<location>/repositories/<name>."
  }
}

variable "network_id" {
  description = "The network the nodes are in."
  type        = string
}

variable "subnet_ids" {
  description = "The three subnets, one per zone."
  type        = list(string)

  validation {
    condition     = length(var.subnet_ids) == 3
    error_message = "Give exactly three subnets, one per zone."
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

variable "kms_key_id" {
  description = "The tenant's Cloud KMS key. It encrypts the disks and the secret, and must be in the region."
  type        = string
}

variable "labels" {
  description = "Labels for every resource that takes them."
  type        = map(string)
  default     = {}
}

variable "reuse_private_access" {
  description = "Use the private endpoints and DNS zones that already exist in this network (the environment's private-access layer) and make none. Leave false in a network that has none, such as a customer's own."
  type        = bool
  default     = false
}
