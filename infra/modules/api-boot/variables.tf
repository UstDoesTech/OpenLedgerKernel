variable "name" {
  description = "The API's name. It names the container and the unit."
  type        = string
}

variable "cell" {
  description = "The cell this API runs next to, as the service knows it (LEDGER_NODE), for example \"aws-lon-03\". Lower-case letters, digits and hyphens."
  type        = string

  validation {
    condition     = can(regex("^[a-z][a-z0-9-]{1,40}$", var.cell))
    error_message = "A cell name is lower-case letters, digits and hyphens, starting with a letter."
  }
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
  description = "The API image, in the private registry, by digest: registry.example/repo/name@sha256:<64 hex digits>. A tag is refused, because a tag can change under a running cell."
  type        = string

  validation {
    condition     = can(regex("^[a-z0-9]([a-z0-9.-]*[a-z0-9])?(:[0-9]+)?(/[a-z0-9._-]+)+@sha256:[0-9a-f]{64}$", var.image))
    error_message = "The image must be given by digest, like registry.example/repo/name@sha256:<64 hex digits>. Tags are refused."
  }
}

variable "secret" {
  description = "The cloud secret that holds this cell's API secrets, as a JSON object with one entry for each file. It is a reference: the machine reads the contents with its own identity at boot, and they never pass through OpenTofu."
  type        = string
}

variable "etcd_endpoints" {
  description = "The etcd client addresses, https only, for example [\"https://10.0.0.1:2379\"]. Addresses, not credentials."
  type        = list(string)

  validation {
    condition     = length(var.etcd_endpoints) > 0 && alltrue([for e in var.etcd_endpoints : can(regex("^https://[^\\s,]+$", e))])
    error_message = "Give at least one etcd address, each starting with https://. etcd here requires TLS."
  }
}

variable "etcd_prefix" {
  description = "The control plane's key prefix in etcd."
  type        = string
  default     = "/ledger"

  validation {
    condition     = can(regex("^/[A-Za-z0-9/_-]*$", var.etcd_prefix))
    error_message = "The prefix starts with a slash and has no spaces."
  }
}

variable "public_port" {
  description = "The port for customers' calls. The service serves TLS on it."
  type        = number
  default     = 8443
}

variable "internal_port" {
  description = "The port for other cells' calls. The service serves mutual TLS on it."
  type        = number
  default     = 8444
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
