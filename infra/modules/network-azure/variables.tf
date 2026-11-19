variable "name" {
  description = "Name prefix for every resource, for example \"ledger-uk\". It is also the resource group's name."
  type        = string
}

variable "location" {
  description = "The Azure region for the network. It comes from modules/regions."
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

variable "network_watcher_name" {
  description = "The region's existing Network Watcher. Null uses the name Azure gives the one it creates: NetworkWatcher_ followed by the location."
  type        = string
  default     = null
}

variable "network_watcher_resource_group_name" {
  description = "The resource group that holds the region's Network Watcher."
  type        = string
  default     = "NetworkWatcherRG"
}

variable "flow_log_retention_days" {
  description = "How long flow logs are kept, in days."
  type        = number
  default     = 365
}

variable "tags" {
  description = "Extra tags for every resource."
  type        = map(string)
  default     = {}
}
