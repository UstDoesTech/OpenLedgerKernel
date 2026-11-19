# What a machine with no internet route needs in order to pull its image and use its secret:
# private endpoints for the container registry (its API and its image host), the secret store,
# and object storage (the registry keeps image layers there). Nothing here is public. The
# machines' own security group gets one extra rule, to these endpoints and nowhere else.
data "aws_prefix_list" "s3" {
  name = "com.amazonaws.${var.region}.s3"
}

resource "aws_security_group" "endpoints" {
  name        = "${var.name}-endpoints"
  description = "Private endpoints for the registry and the secret store"
  vpc_id      = var.vpc_id
  tags        = var.tags
}

resource "aws_vpc_security_group_ingress_rule" "endpoints" {
  for_each = toset(var.client_cidrs)

  security_group_id = aws_security_group.endpoints.id
  description       = "HTTPS from the nodes"
  cidr_ipv4         = each.value
  from_port         = 443
  to_port           = 443
  ip_protocol       = "tcp"
}

resource "aws_vpc_endpoint" "interface" {
  for_each = toset(["ecr.api", "ecr.dkr", "secretsmanager"])

  vpc_id              = var.vpc_id
  service_name        = "com.amazonaws.${var.region}.${each.value}"
  vpc_endpoint_type   = "Interface"
  subnet_ids          = var.subnet_ids
  security_group_ids  = [aws_security_group.endpoints.id]
  private_dns_enabled = true
  tags                = merge(var.tags, { Name = "${var.name}-${each.value}" })
}

resource "aws_vpc_endpoint" "s3" {
  vpc_id            = var.vpc_id
  service_name      = "com.amazonaws.${var.region}.s3"
  vpc_endpoint_type = "Gateway"
  route_table_ids   = [var.route_table_id]
  tags              = merge(var.tags, { Name = "${var.name}-s3" })
}

# The machines' side: HTTPS to the endpoints and to object storage, and nothing else.
resource "aws_security_group" "clients" {
  name        = "${var.name}-endpoint-clients"
  description = "Lets the nodes reach the private endpoints"
  vpc_id      = var.vpc_id
  tags        = var.tags
}

resource "aws_vpc_security_group_egress_rule" "to_endpoints" {
  security_group_id            = aws_security_group.clients.id
  description                  = "HTTPS to the private endpoints"
  referenced_security_group_id = aws_security_group.endpoints.id
  from_port                    = 443
  to_port                      = 443
  ip_protocol                  = "tcp"
}

resource "aws_vpc_security_group_egress_rule" "to_s3" {
  security_group_id = aws_security_group.clients.id
  description       = "HTTPS to object storage, through the gateway endpoint"
  prefix_list_id    = data.aws_prefix_list.s3.id
  from_port         = 443
  to_port           = 443
  ip_protocol       = "tcp"
}
