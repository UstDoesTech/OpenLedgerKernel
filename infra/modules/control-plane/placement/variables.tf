variable "members" {
  description = "How many etcd members. Five by default. Three is the least: etcd needs a majority, and with fewer than three one failure stops it. Four is refused, because losing the failure domain that holds two of them would leave two of four, which is not a majority."
  type        = number
  default     = 5

  validation {
    condition     = var.members >= 3 && floor(var.members) == var.members
    error_message = "The control plane needs at least three members, as a whole number."
  }

  validation {
    condition     = var.members < 3 || var.members - ceil(var.members / 3) >= floor(var.members / 2) + 1
    error_message = "With this many members, losing one failure domain would leave no majority. Use three, five or more than five."
  }
}

variable "clouds" {
  description = "The clouds the control plane may use. All three by default, and each cloud is a failure domain. A geography with one region has one cloud, and then the three availability zones are the failure domains. Two clouds are refused: they cannot spread members so that losing either keeps a majority."
  type        = list(string)
  default     = ["aws", "azure", "gcp"]

  validation {
    condition     = length(setsubtract(var.clouds, ["aws", "azure", "gcp"])) == 0 && length(toset(var.clouds)) == length(var.clouds)
    error_message = "Clouds must be from aws, azure and gcp, each once."
  }

  validation {
    condition     = length(var.clouds) == 1 || length(var.clouds) == 3
    error_message = "Give all three clouds, or one cloud when the geography has one region. Two clouds cannot keep a majority when either is lost."
  }
}

variable "name" {
  description = "The control plane's name. It prefixes each member's full name."
  type        = string
  default     = "ledger-cp"
}

variable "cidrs" {
  description = "The control plane tier's three subnet ranges for each cloud, in zone order. A member takes host number 10 and up in its zone's range, so its address is known before any machine exists. A cloud that is not used may be left out."
  type        = map(list(string))
  default     = {}

  validation {
    condition     = alltrue([for c, list in var.cidrs : length(list) == 3])
    error_message = "Each cloud needs exactly three ranges, one per zone."
  }
}
