# An internet-facing application load balancer in front of one cell's API (D-22). It takes HTTPS on
# 443 only, on a name the owner gives, with a certificate from AWS Certificate Manager. It passes
# traffic to the API machine over HTTPS on the API's own port and checks /healthz. It takes
# connections only from the ranges the owner lists, with a rate limit for each address from AWS WAF.
#
# It is made in two steps, because a certificate can only be used once its name is proved.
# With certificate_validated false it makes the certificate and prints the DNS record that proves
# the name. With it true, after that record exists, it makes everything else.
#
# The load balancer does not check the machine's certificate, so the private authority's
# certificate on the machine works behind it.
locals {
  tags = merge(var.tags, { managed_by = "opentofu", component = "api-front" })
  live = var.certificate_validated ? 1 : 0
}

resource "aws_acm_certificate" "this" {
  domain_name       = var.domain_name
  validation_method = "DNS"
  tags              = local.tags

  lifecycle {
    create_before_destroy = true
  }
}

# Waits until the owner has made the record and the certificate is issued.
resource "aws_acm_certificate_validation" "this" {
  count = local.live

  certificate_arn = aws_acm_certificate.this.arn

  timeouts {
    create = "30m"
  }
}

resource "aws_lb" "this" {
  count = local.live

  name                       = substr(var.name, 0, 32)
  load_balancer_type         = "application"
  internal                   = false
  ip_address_type            = "ipv4"
  subnets                    = var.edge_subnet_ids
  security_groups            = [var.edge_security_group_id]
  drop_invalid_header_fields = true
  desync_mitigation_mode     = "strictest"
  tags                       = merge(local.tags, { Name = var.name })

  lifecycle {
    precondition {
      condition     = var.internet_gateway_id != ""
      error_message = "The edge tier's internet gateway must exist first. Turn on edge_enabled."
    }
  }
}

resource "aws_lb_target_group" "api" {
  count = local.live

  name_prefix      = "api-"
  vpc_id           = var.vpc_id
  target_type      = "instance"
  protocol         = "HTTPS"
  port             = var.api_port
  protocol_version = "HTTP1"

  # The service answers /healthz only when the cell is ready, so a cell that is draining or not
  # ready takes no traffic.
  health_check {
    enabled             = true
    protocol            = "HTTPS"
    port                = tostring(var.api_port)
    path                = "/healthz"
    matcher             = "200"
    interval            = 15
    timeout             = 5
    healthy_threshold   = 2
    unhealthy_threshold = 2
  }

  deregistration_delay = 30
  tags                 = local.tags

  lifecycle {
    create_before_destroy = true
  }
}

resource "aws_lb_target_group_attachment" "api" {
  count = local.live

  target_group_arn = aws_lb_target_group.api[0].arn
  target_id        = var.target_instance_id
  port             = var.api_port
}

# 443 only. There is no listener on port 80, so nothing is served, or redirected, in the clear.
resource "aws_lb_listener" "https" {
  count = local.live

  load_balancer_arn = aws_lb.this[0].arn
  port              = 443
  protocol          = "HTTPS"
  ssl_policy        = "ELBSecurityPolicy-TLS13-1-2-Res-2021-06"
  certificate_arn   = aws_acm_certificate_validation.this[0].certificate_arn
  tags              = local.tags

  default_action {
    type             = "forward"
    target_group_arn = aws_lb_target_group.api[0].arn
  }
}

# The edge group is default-deny. These are the only rules it gets.
resource "aws_vpc_security_group_ingress_rule" "clients" {
  for_each = var.certificate_validated ? toset(var.edge_client_cidrs) : toset([])

  security_group_id = var.edge_security_group_id
  cidr_ipv4         = each.value
  ip_protocol       = "tcp"
  from_port         = 443
  to_port           = 443
  description       = "Allow HTTPS from a named public range"
}

resource "aws_vpc_security_group_ingress_rule" "open" {
  count = var.certificate_validated && var.open_to_internet ? 1 : 0

  security_group_id = var.edge_security_group_id
  cidr_ipv4         = "0.0.0.0/0"
  ip_protocol       = "tcp"
  from_port         = 443
  to_port           = 443
  description       = "Allow HTTPS from any address, behind the rate limit of the web ACL"
}

resource "aws_vpc_security_group_egress_rule" "to_api" {
  count = local.live

  security_group_id            = var.edge_security_group_id
  referenced_security_group_id = var.api_security_group_id
  ip_protocol                  = "tcp"
  from_port                    = var.api_port
  to_port                      = var.api_port
  description                  = "Send traffic and health checks to the port of the API, and nowhere else"
}

resource "aws_vpc_security_group_ingress_rule" "from_load_balancer" {
  count = local.live

  security_group_id            = var.api_security_group_id
  referenced_security_group_id = var.edge_security_group_id
  ip_protocol                  = "tcp"
  from_port                    = var.api_port
  to_port                      = var.api_port
  description                  = "Allow the port of the API from the load balancer"
}

# A rate limit for each address. WAF counts the address the load balancer saw.
resource "aws_wafv2_web_acl" "this" {
  count = local.live

  name  = var.name
  scope = "REGIONAL"
  tags  = local.tags

  default_action {
    allow {}
  }

  rule {
    name     = "rate-limit-each-address"
    priority = 1

    action {
      block {}
    }

    statement {
      rate_based_statement {
        limit                 = var.rate_limit
        aggregate_key_type    = "IP"
        evaluation_window_sec = var.rate_window_seconds
      }
    }

    visibility_config {
      cloudwatch_metrics_enabled = true
      metric_name                = "rate-limit-each-address"
      sampled_requests_enabled   = false
    }
  }

  visibility_config {
    cloudwatch_metrics_enabled = true
    metric_name                = "api-front"
    sampled_requests_enabled   = false
  }
}

resource "aws_wafv2_web_acl_association" "this" {
  count = local.live

  resource_arn = aws_lb.this[0].arn
  web_acl_arn  = aws_wafv2_web_acl.this[0].arn
}
