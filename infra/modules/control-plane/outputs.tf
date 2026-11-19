output "endpoints" {
  description = "The client endpoints of every member, https://address:2379. Clients need a certificate from the same authority."
  value       = [for m in local.members : "https://${m.address}:2379"]
}

output "members" {
  description = "Every member: name, cloud, zone number and address."
  value       = [for m in local.members : { name = m.full_name, cloud = m.cloud, zone = m.zone, address = m.address }]
}

output "quorum" {
  description = "The majority etcd needs."
  value       = module.placement.quorum
}

output "survives_any_single_loss" {
  description = "True when losing any one cloud (or, with one cloud, any one zone) leaves a majority."
  value       = module.placement.survives_any_single_loss
}

output "certificate_secrets" {
  description = "The secret that each member reads its certificates from, by member name. Whoever runs the certificate authority puts a JSON object there with ca.crt, server.crt, server.key, peer.crt and peer.key. They name no certificate and hold none."
  value = merge(
    try(module.aws_members[0].secret_refs, {}),
    try(module.azure_members[0].secret_refs, {}),
    try(module.gcp_members[0].secret_refs, {}),
  )
}
