variable "name" {
  description = "The API's name, normally the cell's name with \"-api\" after it. It names the machine and the secret."
  type        = string
}

variable "cell" {
  description = "The cell this API runs next to, as the service knows it (LEDGER_NODE)."
  type        = string
}

variable "vm_size" {
  description = "The machine size."
  type        = string
  default     = "Standard_B1ms"
}

variable "image" {
  description = "The API image in the private registry, by digest. A tag is refused."
  type        = string

  validation {
    condition     = can(regex("@sha256:[0-9a-f]{64}$", var.image))
    error_message = "The image must be given by digest, like registry.example/repo/name@sha256:<64 hex digits>. Tags are refused."
  }
}

variable "operating_system_image_id" {
  description = "The Flatcar Container Linux image, as a resource id."
  type        = string
}

variable "location" {
  description = "The Azure region. It comes from modules/regions."
  type        = string
}

variable "resource_group_name" {
  description = "The resource group of the network."
  type        = string
}

variable "subnet_id" {
  description = "The private subnet the machine goes in."
  type        = string
}

variable "zone" {
  description = "The availability zone, 1, 2 or 3."
  type        = string
  default     = "1"

  validation {
    condition     = contains(["1", "2", "3"], var.zone)
    error_message = "The zone must be 1, 2 or 3."
  }
}

variable "registry_id" {
  description = "The private registry. The machine may pull from it."
  type        = string
}

variable "key_vault_id" {
  description = "The tenant's Key Vault. It holds the cell's API secret, which the machine may read."
  type        = string
}

variable "tenant_id" {
  description = "The Azure tenant that the machine's identity belongs to."
  type        = string
}

variable "disk_encryption_set_id" {
  description = "The disk encryption set that uses the tenant's Key Vault key."
  type        = string
}

variable "administrator_ssh_public_key" {
  description = "An SSH public key for the machine's operating system account. Sign-in by password is off."
  type        = string
}

variable "etcd_endpoints" {
  description = "The etcd client addresses, https only."
  type        = list(string)
}

variable "tags" {
  description = "Tags for every resource."
  type        = map(string)
  default     = {}
}

variable "migrate_database" {
  description = "Run `ledger-api migrate` once before the API starts (K-25). Off by default."
  type        = bool
  default     = false
}
