variable "azure_subscription_id" {
  description = "The Azure subscription that holds the platform. It names an account, it is not a credential. Supply it as the TF_VAR_azure_subscription_id secret."
  type        = string
}

variable "state_bucket" {
  description = "The bucket that holds the state, to read the earlier layers. It names a bucket, it is not a credential. Supply it as the TF_VAR_state_bucket secret."
  type        = string
}

variable "azure_flatcar_image_id" {
  description = "The Flatcar Container Linux image in Azure, as a resource id of a managed or gallery image in the deploying subscription and the geo's region. Supply it as the TF_VAR_azure_flatcar_image_id secret."
  type        = string
}

variable "administrator_ssh_public_key" {
  description = "The SSH public key for the Azure machines' administrator account. A public key, but marked sensitive so a plan does not print it. Supply it as the TF_VAR_administrator_ssh_public_key secret."
  type        = string
  sensitive   = true
}

variable "client_cidrs" {
  description = "Private address ranges that may call the API's public port, 8443, for example a VPN's range or a test machine's subnet. None by default, so nothing but the platform's own tiers can reach the machines. Each must be inside 10.0.0.0/8, 172.16.0.0/12 or 192.168.0.0/16, and never the whole range. Not a secret."
  type        = list(string)
  default     = []

  validation {
    condition = alltrue([
      for c in var.client_cidrs :
      can(cidrhost(c, 0)) && tonumber(split("/", c)[1]) >= 8 && (
        startswith(c, "10.") || startswith(c, "192.168.") || can(regex("^172\\.(1[6-9]|2[0-9]|3[01])\\.", c))
      )
    ])
    error_message = "Each client range must be a private range (10/8, 172.16/12 or 192.168/16) with a prefix of /8 or longer."
  }
}

variable "edge_enabled" {
  description = "Whether to make the internet gateway and the route table for the edge subnets (D-22). Off by default, so the AWS network has no route to the internet. The load balancer's module needs it on. Not a secret."
  type        = bool
  default     = false
}
