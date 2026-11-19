variable "name" {
  description = "Name prefix for every resource, for example \"ledger-uk\"."
  type        = string
}

variable "mechanism" {
  description = "How the three networks are joined. Only \"ipsec_vpn\", site-to-site IPsec VPN with BGP, is built today. Other mechanisms, such as a private interconnect, are added here as further values."
  type        = string
  default     = "ipsec_vpn"

  validation {
    condition     = contains(["ipsec_vpn"], var.mechanism)
    error_message = "The mechanism must be \"ipsec_vpn\"."
  }
}

variable "plans" {
  description = "Each network's address plan from modules/address-plan, keyed aws, azure and gcp. Each holds its CIDR, its subnets by tier and its gateway subnet."
  type = map(object({
    cidr           = string
    subnets        = map(list(string))
    gateway_subnet = string
  }))

  validation {
    condition     = alltrue([for cloud in ["aws", "azure", "gcp"] : contains(keys(var.plans), cloud)])
    error_message = "plans must have an entry for each of aws, azure and gcp."
  }
}

variable "aws" {
  description = "The AWS network from modules/network-aws: its VPC, its route table and the security group of each tier."
  type = object({
    vpc_id             = string
    route_table_id     = string
    security_group_ids = map(string)
  })
}

variable "azure" {
  description = "The Azure network from modules/network-azure: its region, resource group, virtual network and the network security group name of each tier."
  type = object({
    location                     = string
    resource_group_name          = string
    network_name                 = string
    network_security_group_names = map(string)
  })
}

variable "gcp" {
  description = "The Google Cloud network from modules/network-gcp: its region and the network's name."
  type = object({
    region       = string
    network_name = string
  })
}

variable "preshared_keys" {
  description = "The IPsec pre-shared key of every tunnel, by name. The required names are in the required_preshared_key_names output. Each key is 32 to 64 letters and digits, and no two are the same. Supply them from a secret store when applying, never from a file in this repository."
  type        = map(string)
  sensitive   = true

  validation {
    condition     = alltrue([for key in values(var.preshared_keys) : can(regex("^[0-9A-Za-z]{32,64}$", key))])
    error_message = "Every pre-shared key must be 32 to 64 letters and digits."
  }
}

variable "tags" {
  description = "Extra tags for the AWS and Azure resources."
  type        = map(string)
  default     = {}
}
