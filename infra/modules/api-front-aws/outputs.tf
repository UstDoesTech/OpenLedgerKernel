output "certificate_validation_records" {
  description = "The DNS records the owner must make, at whoever hosts the name, to prove it and so let the certificate be issued. Each is a CNAME. No account identifier is in them."
  value = [
    for o in aws_acm_certificate.this.domain_validation_options : {
      name  = o.resource_record_name
      type  = o.resource_record_type
      value = o.resource_record_value
    }
  ]
}

output "load_balancer_dns_name" {
  description = "The name the page's own record (a CNAME) must point at. Empty until certificate_validated is true."
  value       = var.certificate_validated ? aws_lb.this[0].dns_name : ""
}

output "web_acl_attached" {
  description = "Whether the rate-limiting web ACL is attached to the load balancer."
  value       = var.certificate_validated
}
