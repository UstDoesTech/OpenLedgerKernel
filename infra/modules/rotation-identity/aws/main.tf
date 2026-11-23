# The role the rotate-credentials workflow assumes. Only that GitHub environment can assume it, and it
# may do only what infra/rotation-identity.json lists, on the resources named here. The same
# document is the role's policy and its permissions boundary, so a later edit to the policy cannot
# give it more than the boundary allows, and changing the boundary is a separate right that the
# applying identity does not hold (decision D-18, docs/credentialed-workflows.md).
locals {
  aws = jsondecode(file("${path.module}/../../../rotation-identity.json")).aws

  state_objects = [for key in var.state_keys : "arn:aws:s3:::${var.state_bucket}/${key}"]
  plan_objects  = var.plan_store_prefix == "" ? "arn:aws:s3:::${var.plan_store_bucket}/*" : "arn:aws:s3:::${var.plan_store_bucket}/${var.plan_store_prefix}/*"

  statements = concat(
    [
      {
        Sid      = "Sign"
        Effect   = "Allow"
        Action   = local.aws.sign
        Resource = [var.authority_key_arn]
      },
      {
        Sid      = "Secrets"
        Effect   = "Allow"
        Action   = local.aws.secrets
        Resource = [var.authority_certificate_arn, var.api_secret_arn]
      },
      {
        Sid      = "ReadState"
        Effect   = "Allow"
        Action   = local.aws.state
        Resource = local.state_objects
      },
      {
        Sid      = "ListState"
        Effect   = "Allow"
        Action   = local.aws.state_bucket
        Resource = ["arn:aws:s3:::${var.state_bucket}"]
      },
      {
        Sid      = "KeepLogs"
        Effect   = "Allow"
        Action   = local.aws.plan_store
        Resource = [local.plan_objects]
      },
    ],
    [for _ in(var.administrator_secret_arn == "" ? [] : [1]) : {
      Sid      = "ReadTheAdministratorCredential"
      Effect   = "Allow"
      Action   = local.aws.administrator_secret
      Resource = [var.administrator_secret_arn]
    }],
    [for _ in(var.state_encryption_arn == "" ? [] : [1]) : {
      Sid      = "DecryptTheState"
      Effect   = "Allow"
      Action   = local.aws.state_encryption
      Resource = [var.state_encryption_arn]
    }],
    [for _ in(var.plan_store_encryption_arn == "" ? [] : [1]) : {
      Sid      = "EncryptTheLogs"
      Effect   = "Allow"
      Action   = local.aws.plan_store_encryption
      Resource = [var.plan_store_encryption_arn]
    }],
    [for _ in(var.encryption_arn == "" ? [] : [1]) : {
      Sid      = "UseTheSecretsKey"
      Effect   = "Allow"
      Action   = local.aws.encryption
      Resource = [var.encryption_arn]
    }],
  )

  policy = jsonencode({
    Version   = "2012-10-17"
    Statement = local.statements
  })
}

# The GitHub identity provider already exists in the account (docs/credentialed-workflows.md).
data "aws_iam_openid_connect_provider" "github" {
  url = "https://token.actions.githubusercontent.com"
}

resource "aws_iam_policy" "boundary" {
  name        = "ledger-${var.name}-boundary"
  description = "The most the ${var.name} role can ever do."
  policy      = local.policy
}

resource "aws_iam_role" "this" {
  name                 = var.name
  description          = "Rotates the ledger's certificates and cell credentials. Assumed only by the ${var.environment} environment."
  max_session_duration = local.aws.max_session_seconds
  permissions_boundary = aws_iam_policy.boundary.arn

  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect    = "Allow"
      Principal = { Federated = data.aws_iam_openid_connect_provider.github.arn }
      Action    = "sts:AssumeRoleWithWebIdentity"
      Condition = {
        StringEquals = {
          "token.actions.githubusercontent.com:aud" = "sts.amazonaws.com"
          "token.actions.githubusercontent.com:sub" = "${var.subject_prefix}:environment:${var.environment}"
        }
      }
    }]
  })
}

resource "aws_iam_role_policy" "this" {
  name   = var.name
  role   = aws_iam_role.this.id
  policy = local.policy
}
