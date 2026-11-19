variable "members" {
  description = "The members to run in this cloud: name, zone number (0 to 2) and the address they take. Names are unique across all clouds."
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

variable "machine_type" {
  description = "The machine type for each member."
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
  description = "The etcd image in the private registry, by digest. A tag is refused."
  type        = string

  validation {
    condition     = can(regex("@sha256:[0-9a-f]{64}$", var.image))
    error_message = "The image must be given by digest, like registry.example/repo/name@sha256:<64 hex digits>. Tags are refused."
  }
}

variable "operating_system_image" {
  description = "The Flatcar Container Linux image. It is the operating system only: etcd runs as a container on it."
  type        = string
}

variable "registry_repository_id" {
  description = "The private Artifact Registry repository that holds the etcd image, as projects/<project>/locations/<location>/repositories/<name>. The members may read it."
  type        = string

  validation {
    condition     = can(regex("^projects/[^/]+/locations/[^/]+/repositories/[^/]+$", var.registry_repository_id))
    error_message = "Give the repository as projects/<project>/locations/<location>/repositories/<name>."
  }
}

variable "subnet_ids" {
  description = "The control plane tier's three subnets, one per zone."
  type        = list(string)

  validation {
    condition     = length(var.subnet_ids) == 3
    error_message = "Give exactly three subnets, one per zone."
  }
}

variable "kms_key_id" {
  description = "The Cloud KMS key. It encrypts the disks and the certificate secrets, and must be in the region."
  type        = string
}

variable "labels" {
  description = "Labels for every resource that takes them."
  type        = map(string)
  default     = {}
}
