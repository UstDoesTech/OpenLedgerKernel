variable "name" {
  description = "A name for the private endpoints and zones."
  type        = string
}

variable "location" {
  description = "The Azure region. It comes from modules/regions."
  type        = string
}

variable "resource_group_name" {
  description = "The resource group for the endpoints and zones."
  type        = string
}

variable "virtual_network_id" {
  description = "The virtual network the machines are in. The private DNS zones are linked to it."
  type        = string
}

variable "subnet_id" {
  description = "The subnet the private endpoints take their addresses from."
  type        = string
}

variable "registry_id" {
  description = "The private container registry. It must be a Premium registry, the tier that supports private endpoints."
  type        = string
}

variable "key_vault_id" {
  description = "The tenant's Key Vault, which holds the secret."
  type        = string
}

variable "client_groups" {
  description = "The tiers whose machines use the endpoints, by tier: the name of the tier's network security group and its subnets' address ranges. Each group may send HTTPS to the endpoints' subnets, and nothing else here."
  type = map(object({
    network_security_group_name = string
    cidrs                       = list(string)
  }))
}

variable "endpoint_cidrs" {
  description = "The address ranges of the subnets the private endpoints sit in."
  type        = list(string)
}

variable "endpoint_network_security_group_name" {
  description = "The name of the network security group of the endpoints' subnets. It accepts HTTPS from the client tiers."
  type        = string
}

variable "tags" {
  description = "Tags for every resource."
  type        = map(string)
  default     = {}
}
