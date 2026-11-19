variable "name" {
  description = "The cell's name. It is the server's name, which must be unique across Azure."
  type        = string
}

variable "sku_name" {
  description = "The server's size, for example GP_Standard_D2ds_v5. High availability needs General Purpose or Memory Optimized."
  type        = string
}

variable "storage_gb" {
  description = "Storage in gigabytes."
  type        = number
}

variable "location" {
  description = "The Azure region. It comes from modules/regions."
  type        = string
}

variable "resource_group_name" {
  description = "The resource group for the server and its private endpoint."
  type        = string
}

variable "subnet_id" {
  description = "The cell tier subnet that holds the private endpoint."
  type        = string
}

variable "private_dns_zone_id" {
  description = "The private DNS zone from modules/postgres/azure-dns."
  type        = string
}

variable "key_vault_key_id" {
  description = "The tenant's Key Vault key. It encrypts the data at rest."
  type        = string
}

variable "identity_id" {
  description = "A user-assigned identity that may wrap and unwrap with the tenant's key."
  type        = string
}

variable "administrator" {
  description = "The Microsoft Entra principal that administers the server. Password sign-in is off, so this is the only way in."
  type = object({
    principal_name = string
    object_id      = string
    principal_type = string
  })
}

variable "backup_retention_days" {
  description = "How long backups are kept, in days."
  type        = number
}

variable "tags" {
  description = "Tags for every resource."
  type        = map(string)
  default     = {}
}
