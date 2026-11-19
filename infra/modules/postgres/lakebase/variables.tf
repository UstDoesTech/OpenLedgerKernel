variable "name" {
  description = "The cell's name. It names the Lakebase project."
  type        = string
}

variable "compute_units" {
  description = "Compute units for the primary and each standby."
  type        = number
}

variable "administrator_application_id" {
  description = "The application id of the service principal that administers the database. It signs in with OAuth, so there is no password."
  type        = string
}

variable "deletion_protection" {
  description = "Protect the production branch from deletion."
  type        = bool
}
