variable "name" {
  description = "Name prefix for every resource, for example \"ledger-europe\"."
  type        = string
}

variable "vpc_id" {
  description = "The VPC's id, from the network layer."
  type        = string
}

variable "subnet_ids" {
  description = "The edge tier's three subnets, one per availability zone, from the network layer. Only these are put on the route table with the route to the internet."
  type        = list(string)

  validation {
    condition     = length(var.subnet_ids) == 3
    error_message = "The edge tier has exactly three subnets, one per availability zone."
  }
}

variable "tags" {
  description = "Extra tags for every resource."
  type        = map(string)
  default     = {}
}
