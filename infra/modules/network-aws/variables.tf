variable "name" {
  description = "Name prefix for every resource, for example \"ledger-uk\"."
  type        = string
}

variable "region" {
  description = "The AWS region the provider is set to. It comes from modules/regions. A check fails if the provider is set to another region."
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

variable "flow_log_retention_days" {
  description = "How long flow logs are kept, in days. Must be a value CloudWatch Logs accepts."
  type        = number
  default     = 365
}

variable "flow_log_kms_key_arn" {
  description = "KMS key that encrypts the flow log group. Null uses the CloudWatch Logs default."
  type        = string
  default     = null
}

variable "tags" {
  description = "Extra tags for every resource."
  type        = map(string)
  default     = {}
}
