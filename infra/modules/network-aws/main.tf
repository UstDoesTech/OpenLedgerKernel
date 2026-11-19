# A private network in one AWS region: three subnets per tier, one per availability zone, no
# internet gateway, no NAT, no public addresses, and default-deny security groups. Later
# modules add the routes and rules they need. Flow logs go to CloudWatch Logs.
data "aws_availability_zones" "available" {
  state = "available"
}

check "provider_region" {
  data "aws_region" "current" {}

  assert {
    condition     = data.aws_region.current.region == var.region
    error_message = "The AWS provider is set to a different region from var.region."
  }
}

locals {
  azs  = slice(data.aws_availability_zones.available.names, 0, 3)
  tags = merge(var.tags, { managed_by = "opentofu" })

  subnets = merge([
    for tier, cidrs in var.plan.subnets : {
      for zone, cidr in cidrs : "${tier}-${zone}" => { tier = tier, zone = zone, cidr = cidr }
    }
  ]...)
}

resource "aws_vpc" "this" {
  cidr_block           = var.plan.cidr
  enable_dns_support   = true
  enable_dns_hostnames = true
  tags                 = merge(local.tags, { Name = var.name })
}

resource "aws_subnet" "this" {
  for_each = local.subnets

  vpc_id                  = aws_vpc.this.id
  cidr_block              = each.value.cidr
  availability_zone       = local.azs[each.value.zone]
  map_public_ip_on_launch = false
  tags                    = merge(local.tags, { Name = "${var.name}-${each.key}", tier = each.value.tier })
}

# One route table with only the local route. Private links add their routes to it later.
resource "aws_route_table" "private" {
  vpc_id = aws_vpc.this.id
  tags   = merge(local.tags, { Name = "${var.name}-private" })
}

resource "aws_route_table_association" "private" {
  for_each = aws_subnet.this

  subnet_id      = each.value.id
  route_table_id = aws_route_table.private.id
}

# The VPC's default security group keeps no rules, so nothing can use it by accident.
resource "aws_default_security_group" "this" {
  vpc_id = aws_vpc.this.id
  tags   = merge(local.tags, { Name = "${var.name}-default-deny" })
}

# One security group per tier with no rules at all: no ingress and no egress. Modules that
# place databases, etcd and the API add the rules they need against these groups.
resource "aws_security_group" "tier" {
  for_each = toset(keys(var.plan.subnets))

  name        = "${var.name}-${each.key}"
  description = "Default-deny group for the ${each.key} tier. Rules are added by the modules that use it."
  vpc_id      = aws_vpc.this.id
  tags        = merge(local.tags, { Name = "${var.name}-${each.key}", tier = each.key })
}

resource "aws_cloudwatch_log_group" "flow" {
  name              = "/${var.name}/vpc-flow-logs"
  retention_in_days = var.flow_log_retention_days
  kms_key_id        = var.flow_log_kms_key_arn
  tags              = local.tags
}

data "aws_iam_policy_document" "flow_assume" {
  statement {
    actions = ["sts:AssumeRole"]

    principals {
      type        = "Service"
      identifiers = ["vpc-flow-logs.amazonaws.com"]
    }
  }
}

data "aws_iam_policy_document" "flow_write" {
  statement {
    actions   = ["logs:CreateLogStream", "logs:PutLogEvents", "logs:DescribeLogStreams"]
    resources = ["${aws_cloudwatch_log_group.flow.arn}:*"]
  }
}

resource "aws_iam_role" "flow" {
  name               = "${var.name}-vpc-flow-logs"
  assume_role_policy = data.aws_iam_policy_document.flow_assume.json
  tags               = local.tags
}

resource "aws_iam_role_policy" "flow" {
  name   = "write-flow-logs"
  role   = aws_iam_role.flow.id
  policy = data.aws_iam_policy_document.flow_write.json
}

resource "aws_flow_log" "this" {
  vpc_id                   = aws_vpc.this.id
  traffic_type             = "ALL"
  log_destination_type     = "cloud-watch-logs"
  log_destination          = aws_cloudwatch_log_group.flow.arn
  iam_role_arn             = aws_iam_role.flow.arn
  max_aggregation_interval = 60
  tags                     = merge(local.tags, { Name = "${var.name}-vpc" })
}
