variable "name" {
  description = "The API's name, normally the cell's name with \"-api\" after it. It names the machine, the service account and the secret."
  type        = string
}

variable "cell" {
  description = "The cell this API runs next to, as the service knows it (LEDGER_NODE)."
  type        = string
}

variable "machine_type" {
  description = "The machine type."
  type        = string
  default     = "e2-small"
}

variable "image" {
  description = "The API image in the private registry, by digest. A tag is refused."
  type        = string

  validation {
    condition     = can(regex("@sha256:[0-9a-f]{64}$", var.image))
    error_message = "The image must be given by digest, like registry.example/repo/name@sha256:<64 hex digits>. Tags are refused."
  }
}

variable "operating_system_image" {
  description = "The Flatcar Container Linux image, as a self link."
  type        = string
}

variable "region" {
  description = "The Google Cloud region. It comes from modules/regions."
  type        = string
}

variable "subnet_id" {
  description = "The private subnet the machine goes in."
  type        = string
}

variable "kms_key_id" {
  description = "The tenant's Cloud KMS key. It encrypts the secret and the disk."
  type        = string
}

variable "registry_repository_id" {
  description = "The private registry repository, as projects/<project>/locations/<location>/repositories/<repository>. The machine may read it."
  type        = string
}

variable "etcd_endpoints" {
  description = "The etcd client addresses, https only."
  type        = list(string)
}

variable "labels" {
  description = "Labels for every resource."
  type        = map(string)
  default     = {}
}

variable "migrate_database" {
  description = "Run `ledger-api migrate` once before the API starts (K-25). Off by default."
  type        = bool
  default     = false
}
