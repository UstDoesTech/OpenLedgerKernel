output "kind" {
  description = "Who runs this cell's Postgres: lakebase, managed or self_hosted."
  value       = local.kind
}

output "host" {
  description = "The database's private host name or address."
  value = one(concat(
    module.aws_managed[*].host,
    module.azure_managed[*].host,
    module.gcp_managed[*].host,
    module.lakebase[*].host,
    module.aws_self_hosted[*].host,
    module.azure_self_hosted[*].host,
    module.gcp_self_hosted[*].host,
  ))
}

output "port" {
  description = "The database's port."
  value = one(concat(
    module.aws_managed[*].port,
    module.azure_managed[*].port,
    module.gcp_managed[*].port,
    module.lakebase[*].port,
    module.aws_self_hosted[*].port,
    module.azure_self_hosted[*].port,
    module.gcp_self_hosted[*].port,
  ))
}

output "secret_ref" {
  description = "A reference to where the administrator's credential is held or obtained: a secret's identifier, or the identity that signs in. It is never a password or a connection string."
  value = one(concat(
    module.aws_managed[*].secret_ref,
    module.azure_managed[*].secret_ref,
    module.gcp_managed[*].secret_ref,
    module.lakebase[*].secret_ref,
    module.aws_self_hosted[*].secret_ref,
    module.azure_self_hosted[*].secret_ref,
    module.gcp_self_hosted[*].secret_ref,
  ))
}
