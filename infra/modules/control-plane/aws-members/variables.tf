variable "name" {
  description = "The control plane's name. It prefixes the members and their secrets."
  type        = string
}

variable "members" {
  description = "The members to run in this cloud: name, zone number (0 to 2), and the address they take. Names are unique across all clouds."
  type = list(object({
    name    = string
    zone    = number
    address = string
  }))
}

variable "initial_cluster" {
  description = "Every member of the whole cluster as name=https://address:2380, joined by commas."
  type        = string
}

variable "cluster_id" {
  description = "A name for this cluster."
  type        = string
}

variable "instance_type" {
  description = "The machine type for each member."
  type        = string
}

variable "storage_gb" {
  description = "Data volume size in gigabytes."
  type        = number
}

variable "image" {
  description = "The etcd image in the private registry, by digest. A tag is refused."
  type        = string

  validation {
    condition     = can(regex("@sha256:[0-9a-f]{64}$", var.image))
    error_message = "The image must be given by digest, like registry.example/repo/name@sha256:<64 hex digits>. Tags are refused."
  }
}

variable "operating_system_image_id" {
  description = "The Flatcar Container Linux machine image for the region. It is the operating system only: etcd runs as a container on it."
  type        = string
}

variable "registry_repository_arn" {
  description = "The private registry repository that holds the etcd image. The members may read it."
  type        = string
}

variable "region" {
  description = "The AWS region. It comes from modules/regions."
  type        = string
}

variable "subnet_ids" {
  description = "The control plane tier's three subnets, one per availability zone."
  type        = list(string)

  validation {
    condition     = length(var.subnet_ids) == 3
    error_message = "Give exactly three subnets, one per availability zone."
  }
}

variable "security_group_ids" {
  description = "Security groups for the members: the control plane tier's group, and the private-access layer's client group so they reach the registry and the secret store."
  type        = list(string)
}

variable "kms_key_arn" {
  description = "The KMS key that encrypts the volumes and the certificate secrets."
  type        = string
}

variable "tags" {
  description = "Tags for every resource."
  type        = map(string)
  default     = {}
}
