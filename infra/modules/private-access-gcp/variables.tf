variable "name" {
  description = "A name for the endpoint, the zones and the address."
  type        = string
}

variable "network_id" {
  description = "The network the machines are in."
  type        = string
}

variable "labels" {
  description = "Labels for every resource that takes them."
  type        = map(string)
  default     = {}
}
