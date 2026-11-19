variable "name" {
  description = "The control plane's name. It prefixes every member and secret."
  type        = string
  default     = "ledger-cp"

  validation {
    condition     = can(regex("^[a-z][a-z0-9-]{1,17}$", var.name))
    error_message = "The name must start with a letter, use lower case letters, digits and hyphens, and be 2 to 18 characters."
  }
}

variable "members" {
  description = "How many etcd members. Five by default, placed two, two and one over the three clouds. At least three. Four is refused (see modules/control-plane/placement)."
  type        = number
  default     = 5
}

variable "clouds" {
  description = "The clouds to use: all three by default, or one where the geography has one region."
  type        = list(string)
  default     = ["aws", "azure", "gcp"]
}

variable "storage_gb" {
  description = "Data volume size for each member, in gigabytes."
  type        = number
  default     = 50
}

variable "machine_sizes" {
  description = "The machine size for a member on each cloud. etcd needs fast disks and a steady processor more than memory."
  type = object({
    aws   = string
    azure = string
    gcp   = string
  })
  default = {
    aws   = "m6i.large"
    azure = "Standard_D2s_v5"
    gcp   = "n2-standard-2"
  }
}

variable "tags" {
  description = "Extra tags and labels for every resource."
  type        = map(string)
  default     = {}
}

variable "aws" {
  description = "The AWS network and key for the control plane tier: its three subnets in zone order with their ranges, its security group, the KMS key, the etcd image by digest, the Flatcar machine image, the registry repository, and the network details the private endpoints need. Needed when AWS is one of the clouds."
  type = object({
    subnet_ids                = list(string)
    subnet_cidrs              = list(string)
    security_group_ids        = list(string)
    kms_key_arn               = string
    image                     = string
    operating_system_image_id = string
    registry_repository_arn   = string
    vpc_id                    = string
    route_table_id            = string
    region                    = string
  })
  default = null
}

variable "azure" {
  description = "The Azure network and key for the control plane tier: its three subnets in zone order, a disk encryption set, a Key Vault for the certificate secrets, an SSH public key, the etcd image by digest, the Flatcar machine image, the private registry, and the virtual network and tenant the private endpoints and sign-in need. Needed when Azure is one of the clouds."
  type = object({
    location                     = string
    resource_group_name          = string
    subnet_ids                   = list(string)
    subnet_cidrs                 = list(string)
    disk_encryption_set_id       = string
    key_vault_id                 = string
    administrator_ssh_public_key = string
    image                        = string
    operating_system_image_id    = string
    registry_id                  = string
    virtual_network_id           = string
    tenant_id                    = string
  })
  default = null
}

variable "gcp" {
  description = "The Google Cloud network and key for the control plane tier: the region, the network, its three subnets in zone order, the Cloud KMS key, the etcd image by digest, the Flatcar machine image and the registry repository. Needed when Google Cloud is one of the clouds."
  type = object({
    region                 = string
    network_id             = string
    subnet_ids             = list(string)
    subnet_cidrs           = list(string)
    kms_key_id             = string
    image                  = string
    operating_system_image = string
    registry_repository_id = string
  })
  default = null
}
