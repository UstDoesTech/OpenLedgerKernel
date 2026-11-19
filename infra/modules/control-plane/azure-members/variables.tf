variable "name" {
  description = "The control plane's name. It prefixes the members' resources."
  type        = string
}

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

variable "vm_size" {
  description = "The machine size for each member."
  type        = string
}

variable "storage_gb" {
  description = "Data disk size in gigabytes."
  type        = number
}

variable "location" {
  description = "The Azure region. It comes from modules/regions."
  type        = string
}

variable "resource_group_name" {
  description = "The resource group for the members."
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

variable "operating_system_image_id" {
  description = "The Flatcar Container Linux image. It is the operating system only: etcd runs as a container on it."
  type        = string
}

variable "registry_id" {
  description = "The private container registry (Premium tier) that holds the etcd image. The members may pull from it."
  type        = string
}

variable "tenant_id" {
  description = "The Entra tenant the members' identities belong to. The registry sign-in needs it."
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

variable "disk_encryption_set_id" {
  description = "The disk encryption set that uses a Key Vault key."
  type        = string
}

variable "key_vault_id" {
  description = "The Key Vault that holds the members' certificate secrets."
  type        = string
}

variable "administrator_ssh_public_key" {
  description = "An SSH public key for the machines' operating system account. Sign-in by password is off."
  type        = string
}

variable "tags" {
  description = "Tags for every resource."
  type        = map(string)
  default     = {}
}
