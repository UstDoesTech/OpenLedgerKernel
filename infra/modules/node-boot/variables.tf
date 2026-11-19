variable "name" {
  description = "The node's name. It names the container and the unit."
  type        = string
}

variable "cloud" {
  description = "The cloud the machine runs in: aws, azure or gcp. It picks how the machine signs in to the private registry."
  type        = string

  validation {
    condition     = contains(["aws", "azure", "gcp"], var.cloud)
    error_message = "The cloud must be aws, azure or gcp."
  }
}

variable "image" {
  description = "The node image, in the private registry, by digest: registry.example/repo/name@sha256:<64 hex digits>. A tag is refused, because a tag can change under a running cell."
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

variable "node_config" {
  description = "The node's configuration, as JSON, for the node agent. It names the secret store and the secret, never a credential."
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

variable "tls_secret" {
  description = "The cloud secret that holds this node's certificates and keys, as a JSON object with one entry for each file. It is a reference: the machine reads the contents with its own identity at boot, and they never pass through OpenTofu."
  type        = string
}

variable "tls_files" {
  description = "The files the node needs from that secret: server.crt, server.key and ca.crt, and for a standby also replication.crt and replication.key."
  type        = list(string)

  validation {
    condition     = length(setsubtract(["server.crt", "server.key", "ca.crt"], var.tls_files)) == 0 && length(setsubtract(var.tls_files, ["server.crt", "server.key", "ca.crt", "replication.crt", "replication.key"])) == 0
    error_message = "A node needs server.crt, server.key and ca.crt, and may also take replication.crt and replication.key."
  }
}
