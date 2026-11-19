variable "name" {
  description = "The cell's name. It names the machines."
  type        = string
}

variable "tenant" {
  description = "The tenant's id. It goes in the node configuration."
  type        = string
}

variable "vm_size" {
  description = "The machine size for all three nodes."
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
  description = "The resource group for the machines."
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

variable "operating_system_image_id" {
  description = "The Flatcar Container Linux image. It is the operating system only: the node runs as a container on it."
  type        = string
}

variable "registry_id" {
  description = "The private container registry (Premium tier) that holds the node image. The nodes may pull from it."
  type        = string
}

variable "virtual_network_id" {
  description = "The virtual network the nodes are in."
  type        = string
}

variable "tenant_id" {
  description = "The Entra tenant the machines' identities belong to. The registry sign-in needs it."
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

variable "disk_encryption_set_id" {
  description = "The disk encryption set that uses the tenant's Key Vault key."
  type        = string
}

variable "key_vault_id" {
  description = "The tenant's Key Vault. It holds the administrator's secret, which the nodes may read and write."
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

variable "reuse_private_access" {
  description = "Use the private endpoints and DNS zones that already exist in this network (the environment's private-access layer) and make none. Leave false in a network that has none, such as a customer's own."
  type        = bool
  default     = false
}
