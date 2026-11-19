# The etcd members that run on AWS, one machine each, on Flatcar Container Linux. Each runs etcd
# as a container from an image given by digest. They have fixed private addresses, no public
# address, and volumes encrypted with a key. They pull the image from a private registry and read
# their certificates from Secrets Manager, both over private endpoints. Each member has a role of
# its own that reads its own certificate secret and nothing else.
locals {
  members = { for m in var.members : m.name => m }
}

module "boot" {
  source   = "../../etcd-boot"
  for_each = local.members

  name            = each.key
  cloud           = "aws"
  image           = var.image
  storage_gb      = var.storage_gb
  address         = each.value.address
  initial_cluster = var.initial_cluster
  cluster_id      = var.cluster_id
  tls_reference   = aws_secretsmanager_secret.tls[each.key].arn
  aws_region      = var.region
}

# One secret for each member's certificates and keys, as a JSON object with one entry for each
# file. OpenTofu makes the secret and never its contents: whoever issues the certificates puts
# the values in, so they are not in the state.
resource "aws_secretsmanager_secret" "tls" {
  for_each = local.members

  name                    = "${var.name}/tls/${each.key}"
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

data "aws_iam_policy_document" "tls" {
  for_each = local.members

  statement {
    actions   = ["secretsmanager:GetSecretValue"]
    resources = [aws_secretsmanager_secret.tls[each.key].arn]
  }

  statement {
    actions   = ["kms:Decrypt"]
    resources = [var.kms_key_arn]
  }
}

# The registry: a member may sign in and read the etcd image, and nothing else there.
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

resource "aws_iam_role" "member" {
  for_each = local.members

  name               = each.key
  assume_role_policy = data.aws_iam_policy_document.assume.json
  tags               = var.tags
}

resource "aws_iam_role_policy" "tls" {
  for_each = local.members

  name   = "member-certificates"
  role   = aws_iam_role.member[each.key].id
  policy = data.aws_iam_policy_document.tls[each.key].json
}

resource "aws_iam_role_policy" "registry" {
  for_each = local.members

  name   = "etcd-image"
  role   = aws_iam_role.member[each.key].id
  policy = data.aws_iam_policy_document.registry.json
}

resource "aws_iam_instance_profile" "member" {
  for_each = local.members

  name = each.key
  role = aws_iam_role.member[each.key].name
  tags = var.tags
}

resource "aws_instance" "member" {
  for_each = local.members

  ami                         = var.operating_system_image_id
  instance_type               = var.instance_type
  subnet_id                   = var.subnet_ids[each.value.zone]
  private_ip                  = each.value.address
  vpc_security_group_ids      = var.security_group_ids
  associate_public_ip_address = false
  iam_instance_profile        = aws_iam_instance_profile.member[each.key].name
  user_data                   = module.boot[each.key].ignition
  user_data_replace_on_change = false
  tags                        = merge(var.tags, { Name = each.key })

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

# The data disk is its own resource, as on Azure and Google, so replacing a member to roll a new
# image keeps the disk and with it the member's identity in the cluster. Replacing the machine
# detaches the disk, creates the new machine and attaches the disk to it. The machine's boot finds
# the disk by size, and the member's own restart picks it up if it arrives late.
resource "aws_ebs_volume" "data" {
  for_each = local.members

  availability_zone = aws_instance.member[each.key].availability_zone
  type              = "gp3"
  size              = var.storage_gb
  encrypted         = true
  kms_key_id        = var.kms_key_arn
  tags              = merge(var.tags, { Name = "${each.key}-data" })
}

resource "aws_volume_attachment" "data" {
  for_each = local.members

  device_name                    = "/dev/sdf"
  volume_id                      = aws_ebs_volume.data[each.key].id
  instance_id                    = aws_instance.member[each.key].id
  stop_instance_before_detaching = true
}
