# The way in from the internet for the AWS load balancer (D-22), and nothing else. It makes an
# internet gateway and a route table of its own with a default route to it, and puts the three
# edge subnets on that table. No private subnet is on it, so no other tier gets a route to the
# internet, and the private route table (modules/network-aws) is not touched.
#
# It makes no security group rule: the edge tier's group stays default-deny until the load
# balancer's own module adds the rules it needs. The subnets give no public address on launch, so
# a machine put in one is not reachable until it is given one on purpose.
locals {
  tags = merge(var.tags, { managed_by = "opentofu", component = "edge" })
}

resource "aws_internet_gateway" "this" {
  vpc_id = var.vpc_id
  tags   = merge(local.tags, { Name = "${var.name}-edge" })
}

resource "aws_route_table" "edge" {
  vpc_id = var.vpc_id
  tags   = merge(local.tags, { Name = "${var.name}-edge" })
}

resource "aws_route" "internet" {
  route_table_id         = aws_route_table.edge.id
  destination_cidr_block = "0.0.0.0/0"
  gateway_id             = aws_internet_gateway.this.id
}

resource "aws_route_table_association" "edge" {
  count = length(var.subnet_ids)

  subnet_id      = var.subnet_ids[count.index]
  route_table_id = aws_route_table.edge.id
}
