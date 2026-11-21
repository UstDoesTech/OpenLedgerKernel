output "role_arn" {
  description = "The role. It is the AWS_ROLE_ARN secret of the rotate-credentials environment."
  value       = aws_iam_role.this.arn
}

output "boundary_arn" {
  description = "The permissions boundary on the role."
  value       = aws_iam_policy.boundary.arn
}
