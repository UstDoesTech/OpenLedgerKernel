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

variable "front_domain_name" {
  description = "The name the page is served on. Empty means no load balancer. The owner gives it when applying, as the TF_VAR_front_domain_name secret, so it is not written in this public repository."
  type        = string
  default     = ""
}

variable "certificate_validated" {
  description = "False makes only the certificate and prints the DNS record that proves the name. Set it true once that record exists, to make the load balancer. Not a secret."
  type        = bool
  default     = false
}

variable "edge_client_cidrs" {
  description = "Public address ranges that may reach the load balancer on 443, for example one person's /32. None means no ingress rule is made. the whole internet is refused here: use open_to_internet. Not a secret."
  type        = list(string)
  default     = []
}

variable "open_to_internet" {
  description = "Takes connections from any address, with the API told the load balancer's ranges so each address has its own sign-in limit, and the web ACL's rate limit in front. A deliberate switch. Not a secret."
  type        = bool
  default     = false
}

variable "migrate_database" {
  description = "Runs `ledger-api migrate` once before each API machine's service starts, so a cell built empty gets its ledger schema (K-25). Off by default. Switch it on only for the time it takes: it uses the role in db.conn, and a machine takes it only when it is replaced. See docs/runbooks/applying-the-ledger-schema.md. Not a secret."
  type        = bool
  default     = false
}

variable "create_database_role" {
  description = "Makes each managed AWS cell's application role from its API machine, once, before the schema step (K-24cc). Off by default. Switch it on for one apply: while it is on the machine's role can read the database's administrator secret, and a machine takes it only when it is replaced. Switch it off and apply again afterwards. See docs/runbooks/applying-the-ledger-schema.md. Not a secret."
  type        = bool
  default     = false
}
