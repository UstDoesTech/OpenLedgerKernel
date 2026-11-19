variable "name" {
  description = "The cell's name, lower-case letters, digits and hyphens, for example \"uk-aws-001\". It names every resource."
  type        = string

  validation {
    condition     = can(regex("^[a-z][a-z0-9-]{2,40}$", var.name))
    error_message = "The name must be 3 to 41 characters: lower-case letters, digits and hyphens, starting with a letter."
  }
}

variable "tenant" {
  description = "The tenant this cell belongs to, for example \"ten_acme\". Cells are never shared between tenants. It is recorded on every resource, and the encryption key supplied must be this tenant's."
  type        = string

  validation {
    condition     = can(regex("^ten_[0-9a-z]{1,24}$", var.tenant))
    error_message = "A tenant id is \"ten_\" followed by 1 to 24 lower-case letters or digits."
  }
}

variable "cloud" {
  description = "The cloud the cell runs on: aws, azure or gcp. For Lakebase it is the cloud the Databricks workspace runs on."
  type        = string

  validation {
    condition     = contains(["aws", "azure", "gcp"], var.cloud)
    error_message = "The cloud must be aws, azure or gcp."
  }
}

variable "provider_kind" {
  description = "Who runs Postgres: \"lakebase\", \"managed\" (the cloud's own PostgreSQL service) or \"self_hosted\" (virtual machines running the node image as a container). Null means Lakebase where a databricks input is given, and the cloud's managed service otherwise."
  type        = string
  default     = null

  validation {
    condition     = var.provider_kind == null || contains(["lakebase", "managed", "self_hosted"], coalesce(var.provider_kind, "none"))
    error_message = "The provider kind must be lakebase, managed or self_hosted."
  }
}

variable "size" {
  description = "How big the cell is: small or medium. Each maps to a size on every cloud."
  type        = string
  default     = "small"

  validation {
    condition     = contains(["small", "medium"], var.size)
    error_message = "The size must be small or medium."
  }
}

variable "storage_gb" {
  description = "Data storage in gigabytes. Not used by Lakebase, which manages its own."
  type        = number
  default     = 128
}

variable "backup_retention_days" {
  description = "How long backups are kept, in days. Not set for self-hosted cells, which need their own backup arrangement."
  type        = number
  default     = 35
}

variable "deletion_protection" {
  description = "Refuse to delete the database. Switch off only to retire a cell."
  type        = bool
  default     = true
}

variable "tags" {
  description = "Extra tags and labels for every resource."
  type        = map(string)
  default     = {}
}

variable "aws" {
  description = "The AWS network and key, from modules/network-aws and the tenant's key: the cell tier's three subnets in zone order with their ranges, its security group, the tenant's KMS key and, for self-hosted, the container image by digest, the Flatcar machine image, the registry repository and the network details the private endpoints need."
  type = object({
    subnet_ids                = list(string)
    subnet_cidrs              = list(string)
    security_group_ids        = list(string)
    kms_key_arn               = string
    image                     = optional(string)
    operating_system_image_id = optional(string)
    registry_repository_arn   = optional(string)
    vpc_id                    = optional(string)
    route_table_id            = optional(string)
    region                    = optional(string)
  })
  default = null
}

variable "azure" {
  description = "The Azure network and key: the cell tier's three subnets in zone order with their ranges, the shared private DNS zone for PostgreSQL, the tenant's Key Vault key with the user-assigned identity allowed to use it, an Entra administrator and, for self-hosted, a disk encryption set, an SSH public key, the container image by digest, the Flatcar machine image, the private registry and the virtual network and tenant the private endpoints and sign-in need."
  type = object({
    location                     = string
    resource_group_name          = string
    subnet_ids                   = list(string)
    subnet_cidrs                 = list(string)
    private_dns_zone_id          = optional(string)
    key_vault_key_id             = string
    identity_id                  = string
    disk_encryption_set_id       = optional(string)
    key_vault_id                 = optional(string)
    administrator_ssh_public_key = optional(string)
    image                        = optional(string)
    operating_system_image_id    = optional(string)
    registry_id                  = optional(string)
    virtual_network_id           = optional(string)
    tenant_id                    = optional(string)
    administrator = object({
      principal_name = string
      object_id      = string
      principal_type = string
    })
  })
  default = null
}

variable "gcp" {
  description = "The Google Cloud network and key: the region, the network, the cell tier's three subnets in zone order with their ranges, the tenant's Cloud KMS key, an administrator service account and, for self-hosted, the container image by digest, the Flatcar machine image and the registry repository."
  type = object({
    region                 = string
    network_id             = string
    subnet_ids             = list(string)
    subnet_cidrs           = list(string)
    kms_key_id             = string
    administrator_email    = string
    image                  = optional(string)
    operating_system_image = optional(string)
    registry_repository_id = optional(string)
  })
  default = null
}

variable "databricks" {
  description = "For Lakebase: the workspace's administrator service principal, and two statements the caller makes about the tenant's workspace, which this module cannot check: that it uses Private Link and that a customer-managed key configuration is bound to it."
  type = object({
    administrator_application_id = string
    private_link                 = bool
    customer_managed_key         = bool
  })
  default = null
}

variable "reuse_private_access" {
  description = "For self-hosted cells: use the private endpoints and DNS zones that already exist in the network (the environment's private-access layer) and make none. Leave false in a network that has none, such as a customer's own. Add the layer's client security group to the AWS security_group_ids."
  type        = bool
  default     = false
}

variable "machine_sizes" {
  description = "Machine sizes for self-hosted cells, to use in place of the size table when a subscription or region cannot supply those. Each is optional; the table applies to any left out."
  type = object({
    aws   = optional(string)
    azure = optional(string)
    gcp   = optional(string)
  })
  default = {}
}
