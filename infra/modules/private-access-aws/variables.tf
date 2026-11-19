variable "name" {
  description = "A name for the endpoints and their security groups."
  type        = string
}

variable "vpc_id" {
  description = "The VPC the machines are in."
  type        = string
}

variable "region" {
  description = "The AWS region. It comes from modules/regions."
  type        = string
}

variable "subnet_ids" {
  description = "The subnets the interface endpoints sit in."
  type        = list(string)
}

variable "client_cidrs" {
  description = "The ranges of the machines that may reach the endpoints."
  type        = list(string)
}

variable "route_table_id" {
  description = "The private route table. The gateway endpoint for object storage, which holds the registry's image layers, adds a route here."
  type        = string
}

variable "tags" {
  description = "Tags for every resource."
  type        = map(string)
  default     = {}
}
