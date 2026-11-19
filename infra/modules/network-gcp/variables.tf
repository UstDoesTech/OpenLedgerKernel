variable "name" {
  description = "Name prefix for every resource, for example \"ledger-uk\"."
  type        = string
}

variable "region" {
  description = "The Google Cloud region for the subnets. It comes from modules/regions."
  type        = string
}

variable "plan" {
  description = "This network's address plan from modules/address-plan: its CIDR and three subnets, one per availability zone, for each of cell, control_plane and api."
  type = object({
    cidr    = string
    subnets = map(list(string))
  })

  validation {
    condition     = alltrue([for tier in ["cell", "control_plane", "api"] : length(try(var.plan.subnets[tier], [])) == 3])
    error_message = "The plan must give exactly three subnets, one per availability zone, for each of cell, control_plane and api."
  }
}
