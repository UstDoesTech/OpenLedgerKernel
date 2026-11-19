# Three machines, one per zone, on Flatcar Container Linux. Each runs the node image, given by
# digest, as a container. They have fixed private addresses, no public address, and volumes
# encrypted with the tenant's key. They pull the image from a private registry and use the
# secret store, both over private endpoints. The first node starts as the primary and commits
# when one of the other two has the change (ANY 1). Its first start creates the administrator's
# credential and stores it in the secret made here, using the machine's own role, which may
# read and write that secret and nothing else.
# There is no automatic failover in this form: promoting a standby is a manual step.
locals {
  addresses = [for cidr in var.subnet_cidrs : cidrhost(cidr, 10)]
  names     = [for i in range(3) : "${var.name}-n${i + 1}"]

  node_config = [for i in range(3) : jsonencode({
    tenant                    = var.tenant
    node                      = local.names[i]
    role                      = i == 0 ? "primary" : "standby"
    postgres_version          = 16
    primary_address           = local.addresses[0]
    synchronous_standby_names = i == 0 ? "ANY 1 (${join(", ", slice(local.names, 1, 3))})" : ""
    secret_id                 = aws_secretsmanager_secret.administrator.arn
    secret_store              = { kind = "aws_secrets_manager" }
  })]
}

locals {
  tls_files = {
    primary = ["server.crt", "server.key", "ca.crt"]
    standby = ["server.crt", "server.key", "ca.crt", "replication.crt", "replication.key"]
  }
}

module "private_access" {
  source = "../../private-access-aws"
  count  = var.reuse_private_access ? 0 : 1

  name           = var.name
  vpc_id         = var.vpc_id
  region         = var.region
  subnet_ids     = var.subnet_ids
  client_cidrs   = var.subnet_cidrs
  route_table_id = var.route_table_id
  tags           = var.tags
}

module "boot" {
  source = "../../node-boot"
  count  = 3

  name        = local.names[count.index]
  cloud       = "aws"
  image       = var.image
  storage_gb  = var.storage_gb
  node_config = local.node_config[count.index]
  aws_region  = var.region
  tls_secret  = aws_secretsmanager_secret.tls[count.index].arn
  tls_files   = count.index == 0 ? local.tls_files.primary : local.tls_files.standby
}

resource "aws_secretsmanager_secret" "administrator" {
  name                    = "${var.name}/administrator"
  kms_key_id              = var.kms_key_arn
  recovery_window_in_days = 7
  tags                    = var.tags
}

# One secret for each node's certificates and keys, as a JSON object with one entry for each
# file. OpenTofu makes the secret and never its contents: whoever issues the certificates puts
# the values in, so they are not in the state. The node reads its own secret at boot.
resource "aws_secretsmanager_secret" "tls" {
  count = 3

  name                    = "${var.name}/tls/${local.names[count.index]}"
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

data "aws_iam_policy_document" "node" {
  statement {
    actions   = ["secretsmanager:GetSecretValue", "secretsmanager:PutSecretValue"]
    resources = [aws_secretsmanager_secret.administrator.arn]
  }

  statement {
    actions   = ["kms:Decrypt", "kms:GenerateDataKey"]
    resources = [var.kms_key_arn]
  }
}

# The registry: the machine may sign in and read the node image, and nothing else there.
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

# Each node has a role of its own, so that it can read its own certificates and no other node's.
data "aws_iam_policy_document" "tls" {
  count = 3

  statement {
    actions   = ["secretsmanager:GetSecretValue"]
    resources = [aws_secretsmanager_secret.tls[count.index].arn]
  }
}

resource "aws_iam_role_policy" "tls" {
  count = 3

  name   = "node-certificates"
  role   = aws_iam_role.node[count.index].id
  policy = data.aws_iam_policy_document.tls[count.index].json
}

resource "aws_iam_role_policy" "registry" {
  count = 3

  name   = "node-image"
  role   = aws_iam_role.node[count.index].id
  policy = data.aws_iam_policy_document.registry.json
}

resource "aws_iam_role" "node" {
  count = 3

  name               = local.names[count.index]
  assume_role_policy = data.aws_iam_policy_document.assume.json
  tags               = var.tags
}

resource "aws_iam_role_policy" "node" {
  count = 3

  name   = "administrator-secret"
  role   = aws_iam_role.node[count.index].id
  policy = data.aws_iam_policy_document.node.json
}

resource "aws_iam_instance_profile" "node" {
  count = 3

  name = local.names[count.index]
  role = aws_iam_role.node[count.index].name
  tags = var.tags
}

resource "aws_instance" "node" {
  count = 3

  ami                         = var.operating_system_image_id
  instance_type               = var.instance_type
  subnet_id                   = var.subnet_ids[count.index]
  private_ip                  = local.addresses[count.index]
  vpc_security_group_ids      = concat(var.security_group_ids, module.private_access[*].client_security_group_id)
  associate_public_ip_address = false
  iam_instance_profile        = aws_iam_instance_profile.node[count.index].name
  user_data                   = module.boot[count.index].ignition
  user_data_replace_on_change = false
  tags                        = merge(var.tags, { Name = local.names[count.index] })

  metadata_options {
    http_endpoint = "enabled"
    http_tokens   = "required"
    # The container reaches the metadata service one network hop further than the machine.
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

# The data disk is its own resource, as on Azure and Google, so replacing a machine to roll a new
# node image keeps the disk. The machine's boot finds it by size and mounts it; if the disk
# arrives after the machine starts, the node's own restart picks it up.
resource "aws_ebs_volume" "data" {
  count = 3

  availability_zone = aws_instance.node[count.index].availability_zone
  type              = "gp3"
  size              = var.storage_gb
  encrypted         = true
  kms_key_id        = var.kms_key_arn
  tags              = merge(var.tags, { Name = "${local.names[count.index]}-data" })
}

resource "aws_volume_attachment" "data" {
  count = 3

  device_name                    = "/dev/sdf"
  volume_id                      = aws_ebs_volume.data[count.index].id
  instance_id                    = aws_instance.node[count.index].id
  stop_instance_before_detaching = true
}
