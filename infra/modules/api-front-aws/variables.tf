variable "name" {
  description = "Name prefix for every resource, for example \"ledger-europe-aws-api\"."
  type        = string
}

variable "vpc_id" {
  description = "The VPC's id, from the network layer."
  type        = string
}

variable "edge_subnet_ids" {
  description = "The edge tier's three subnets, one per availability zone. The load balancer goes in them."
  type        = list(string)

  validation {
    condition     = length(var.edge_subnet_ids) == 3
    error_message = "The edge tier has exactly three subnets, one per availability zone."
  }
}

variable "edge_security_group_id" {
  description = "The edge tier's security group, default-deny, from the network layer. This module adds the load balancer's rules to it and nothing else."
  type        = string
}

variable "api_security_group_id" {
  description = "The API tier's security group. This module lets the load balancer's group reach the API's public port in it."
  type        = string
}

variable "api_port" {
  description = "The API's public port, where the load balancer sends traffic and checks health."
  type        = number
  default     = 8443
}

variable "target_instance_id" {
  description = "The API machine. Each cell has one, so there is one target."
  type        = string
}

variable "domain_name" {
  description = "The name the page is served on, such as \"page.example.org\". The owner gives it when applying. Never written in the repository, which is public."
  type        = string

  validation {
    condition     = can(regex("^([a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?\\.)+[a-z]{2,63}$", var.domain_name))
    error_message = "Give one lower-case name, such as page.example.org, with no wildcard and no trailing dot."
  }
}

variable "certificate_validated" {
  description = "False makes only the certificate, whose DNS validation record is printed, so the owner can make it. True, once that record exists, waits for the certificate to be issued and makes the load balancer, its listener, its rules and the web ACL."
  type        = bool
  default     = false
}

variable "internet_gateway_id" {
  description = "The edge tier's internet gateway, from the edge module. The load balancer cannot be made without it, so there is never an internet-facing load balancer with no way out to the internet."
  type        = string
  default     = ""
}

variable "edge_client_cidrs" {
  description = "Public address ranges that may reach the load balancer on 443, for example one person's /32. None means no ingress rule is made. 0.0.0.0/0 and ::/0 are refused here: to take the whole internet, say so with open_to_internet. Not a secret."
  type        = list(string)
  default     = []

  validation {
    condition = alltrue([
      for c in var.edge_client_cidrs :
      can(cidrhost(c, 0)) && !strcontains(c, ":") && tonumber(split("/", c)[1]) >= 8
    ])
    error_message = "Each range must be an IPv4 range with a prefix of /8 or longer. 0.0.0.0/0 and ::/0 are refused: use open_to_internet to take the whole internet."
  }
}

variable "open_to_internet" {
  description = "Takes connections from any address. It is a deliberate switch of its own, and the API must have been told which ranges are the load balancer's (LEDGER_TRUSTED_PROXIES, K-23d), so each address has its own sign-in limit. The web ACL's rate limit applies either way."
  type        = bool
  default     = false
}

variable "rate_limit" {
  description = "Requests allowed from one address in the evaluation window before it is blocked."
  type        = number
  default     = 300

  validation {
    condition     = var.rate_limit >= 10 && var.rate_limit <= 20000
    error_message = "The rate limit must be between 10 and 20000."
  }
}

variable "rate_window_seconds" {
  description = "The evaluation window for the rate limit. AWS WAF allows 60, 120, 300 or 600."
  type        = number
  default     = 300

  validation {
    condition     = contains([60, 120, 300, 600], var.rate_window_seconds)
    error_message = "The window must be 60, 120, 300 or 600 seconds."
  }
}

variable "tags" {
  description = "Extra tags for every resource."
  type        = map(string)
  default     = {}
}
