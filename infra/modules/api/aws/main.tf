# One small machine, on Flatcar Container Linux, that runs the API image next to a cell. It has a
# private address and no public one, and a volume encrypted with the tenant's key. Its role may
# read the cell's one API secret, decrypt with the tenant's key and pull the API image, and
# nothing else: not the database's administrator secret, not another cell's secrets.
module "boot" {
  source = "../../api-boot"

  name           = var.name
  cell           = var.cell
  cloud          = "aws"
  image          = var.image
  secret         = aws_secretsmanager_secret.api.arn
  etcd_endpoints = var.etcd_endpoints
  aws_region     = var.region

  trusted_proxy_cidrs = var.trusted_proxy_cidrs
}

# The secret is made empty. Whoever issues the certificates and tokens puts the values in, so
# they are never in OpenTofu's state.
resource "aws_secretsmanager_secret" "api" {
  name                    = "${var.name}/api"
  kms_key_id              = var.kms_key_arn
  recovery_window_in_days = 7
  tags                    = var.tags
}

data "aws_iam_policy_document" "assume" {
  statement {
    actions = ["sts:AssumeRole"]

    principals {
      type        = "Service"
      identifiers = ["ec2.amazonaws.com"]
    }
  }
}

data "aws_iam_policy_document" "secret" {
  statement {
    actions   = ["secretsmanager:GetSecretValue"]
    resources = [aws_secretsmanager_secret.api.arn]
  }

  statement {
    actions   = ["kms:Decrypt"]
    resources = [var.kms_key_arn]
  }
}

# The registry: the machine may sign in and read the API image, and nothing else there.
data "aws_iam_policy_document" "registry" {
  statement {
    actions   = ["ecr:GetAuthorizationToken"]
    resources = ["*"]
  }

  statement {
    actions   = ["ecr:BatchGetImage", "ecr:GetDownloadUrlForLayer", "ecr:BatchCheckLayerAvailability"]
    resources = [var.registry_repository_arn]
  }
}

resource "aws_iam_role" "api" {
  name               = var.name
  assume_role_policy = data.aws_iam_policy_document.assume.json
  tags               = var.tags
}

resource "aws_iam_role_policy" "secret" {
  name   = "api-secret"
  role   = aws_iam_role.api.id
  policy = data.aws_iam_policy_document.secret.json
}

resource "aws_iam_role_policy" "registry" {
  name   = "api-image"
  role   = aws_iam_role.api.id
  policy = data.aws_iam_policy_document.registry.json
}

resource "aws_iam_instance_profile" "api" {
  name = var.name
  role = aws_iam_role.api.name
  tags = var.tags
}

resource "aws_instance" "api" {
  ami                         = var.operating_system_image_id
  instance_type               = var.instance_type
  subnet_id                   = var.subnet_id
  vpc_security_group_ids      = var.security_group_ids
  associate_public_ip_address = false
  iam_instance_profile        = aws_iam_instance_profile.api.name
  user_data                   = module.boot.ignition
  user_data_replace_on_change = false
  tags                        = merge(var.tags, { Name = var.name })

  metadata_options {
    http_endpoint = "enabled"
    http_tokens   = "required"
    # The container runs one network hop further than the machine.
    http_put_response_hop_limit = 2
  }

  root_block_device {
    encrypted  = true
    kms_key_id = var.kms_key_arn
  }

  lifecycle {
    ignore_changes = [ami, user_data]
  }
}
