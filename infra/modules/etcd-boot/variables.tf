variable "name" {
  description = "The member's full name, unique across the clouds. It names the etcd member, the container and the unit."
  type        = string
}

variable "cloud" {
  description = "The cloud the machine runs in: aws, azure or gcp. It picks how the machine signs in to the private registry and reads its secret."
  type        = string

  validation {
    condition     = contains(["aws", "azure", "gcp"], var.cloud)
    error_message = "The cloud must be aws, azure or gcp."
  }
}

variable "image" {
  description = "The etcd image, in the private registry, by digest. A tag is refused, because a tag can change under a running cluster."
  type        = string

  validation {
    condition     = can(regex("^[a-z0-9]([a-z0-9.-]*[a-z0-9])?(:[0-9]+)?(/[a-z0-9._-]+)+@sha256:[0-9a-f]{64}$", var.image))
    error_message = "The image must be given by digest, like registry.example/repo/name@sha256:<64 hex digits>. Tags are refused."
  }
}

variable "storage_gb" {
  description = "The data disk's size in gigabytes. The machine finds its data disk by this exact size, so no other disk may match it."
  type        = number

  validation {
    condition     = var.storage_gb >= 20 && floor(var.storage_gb) == var.storage_gb
    error_message = "The data disk must be a whole number of at least 20 gigabytes."
  }
}

variable "address" {
  description = "The member's private address. Peers and clients reach it here."
  type        = string
}

variable "initial_cluster" {
  description = "Every member as name=https://address:2380, joined by commas. It is the same on every member."
  type        = string
}

variable "cluster_id" {
  description = "A name for this cluster, so members of different clusters never join each other."
  type        = string
}

variable "tls_reference" {
  description = "The cloud secret that holds this member's certificates and keys, as a JSON object with one entry for each file. It is a reference: the machine reads the contents with its own identity at boot, and they never pass through OpenTofu."
  type        = string
}

variable "aws_region" {
  description = "For AWS: the region of the container registry, which the machine signs in to."
  type        = string
  default     = null
}

variable "azure_tenant_id" {
  description = "For Azure: the tenant that the machine's identity belongs to."
  type        = string
  default     = null
}
