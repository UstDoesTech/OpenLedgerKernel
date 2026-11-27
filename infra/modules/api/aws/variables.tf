variable "name" {
  description = "The API's name, normally the cell's name with \"-api\" after it. It names the machine, the role and the secret."
  type        = string
}

variable "cell" {
  description = "The cell this API runs next to, as the service knows it (LEDGER_NODE)."
  type        = string
}

variable "instance_type" {
  description = "The machine type."
  type        = string
  default     = "t3.small"
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
  description = "The Flatcar Container Linux machine image for the region."
  type        = string
}

variable "registry_repository_arn" {
  description = "The private registry repository that holds the API image. The machine may read it."
  type        = string
}

variable "region" {
  description = "The AWS region. It comes from modules/regions."
  type        = string
}

variable "subnet_id" {
  description = "The private subnet the machine goes in."
  type        = string
}

variable "security_group_ids" {
  description = "Security groups for the machine. They must allow the public and internal ports in from the right places, and the private access group, so that the machine reaches the registry and the secret store."
  type        = list(string)
}

variable "kms_key_arn" {
  description = "The tenant's KMS key. It encrypts the secret and the machine's disk, and the machine may decrypt with it."
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

variable "trusted_proxy_cidrs" {
  description = "The ranges a load balancer in front of this machine connects from (the edge subnets). The service believes the forwarded client address only from these, to limit sign-in for each address. None means it never reads the header."
  type        = list(string)
  default     = []
}

variable "migrate_database" {
  description = "Run `ledger-api migrate` once before the API starts (K-25). Off by default."
  type        = bool
  default     = false
}

variable "database_resource_id" {
  description = "The resource id of the cell's managed database cluster, when the cell is managed. The machine's role may then sign in to that one cluster as the one database role below, and as no other. Null for a self-hosted cell, which gets no such grant."
  type        = string
  default     = null
}

variable "database_role" {
  description = "The one database role the machine may sign in as. K-24ca makes it."
  type        = string
  default     = "ledger_app"
}

variable "create_database_role" {
  description = "Runs the one-off step that makes a managed cell's application role from this machine (K-24cc). Off by default. While it is on, the machine's role may read the managed database's administrator secret, and that is the only time it may. Needs administrator_secret_arn. Switch it on for one apply and off again."
  type        = bool
  default     = false

  validation {
    condition     = !var.create_database_role || (var.administrator_secret_arn != null && var.database_resource_id != null)
    error_message = "create_database_role needs the managed cell's administrator_secret_arn and database_resource_id."
  }
}

variable "administrator_secret_arn" {
  description = "The managed database's administrator secret, as RDS holds it. A reference, never the credential. Read only while create_database_role is on."
  type        = string
  default     = null
}
