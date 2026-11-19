variable "resource_group_name" {
  description = "The resource group that holds the network."
  type        = string
}

variable "virtual_network_id" {
  description = "The virtual network that resolves the private zone."
  type        = string
}

variable "name" {
  description = "Name for the zone's link to the network, for example \"ledger-uk\"."
  type        = string
}

variable "tags" {
  description = "Tags for every resource."
  type        = map(string)
  default     = {}
}
