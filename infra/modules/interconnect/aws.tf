# AWS: one virtual private gateway on the VPC, learning routes by BGP into the private route
# table. Each AWS VPN connection carries two tunnels to two AWS endpoints.
resource "aws_vpn_gateway" "this" {
  vpc_id          = var.aws.vpc_id
  amazon_side_asn = local.asn.aws
  tags            = merge(local.tags, { Name = "${var.name}-vgw" })
}

resource "aws_vpn_gateway_route_propagation" "private" {
  vpn_gateway_id = aws_vpn_gateway.this.id
  route_table_id = var.aws.route_table_id
}

# To Azure: one customer gateway (the Azure gateway) and one connection, so two tunnels.
resource "aws_customer_gateway" "azure" {
  bgp_asn    = local.asn.azure
  ip_address = azurerm_public_ip.vpn.ip_address
  type       = "ipsec.1"
  tags       = merge(local.tags, { Name = "${var.name}-azure" })
}

resource "aws_vpn_connection" "azure" {
  vpn_gateway_id      = aws_vpn_gateway.this.id
  customer_gateway_id = aws_customer_gateway.azure.id
  type                = "ipsec.1"
  static_routes_only  = false
  tags                = merge(local.tags, { Name = "${var.name}-azure" })

  tunnel1_inside_cidr                  = local.tunnels["aws-azure"][0]
  tunnel1_preshared_key                = var.preshared_keys["aws-azure-0"]
  tunnel1_ike_versions                 = local.ipsec.ike_versions
  tunnel1_phase1_dh_group_numbers      = local.ipsec.dh_groups
  tunnel1_phase1_encryption_algorithms = local.ipsec.encryption
  tunnel1_phase1_integrity_algorithms  = local.ipsec.integrity
  tunnel1_phase1_lifetime_seconds      = local.ipsec.phase1_lifetime
  tunnel1_phase2_dh_group_numbers      = local.ipsec.dh_groups
  tunnel1_phase2_encryption_algorithms = local.ipsec.encryption
  tunnel1_phase2_integrity_algorithms  = local.ipsec.integrity
  tunnel1_phase2_lifetime_seconds      = local.ipsec.phase2_lifetime

  tunnel2_inside_cidr                  = local.tunnels["aws-azure"][1]
  tunnel2_preshared_key                = var.preshared_keys["aws-azure-1"]
  tunnel2_ike_versions                 = local.ipsec.ike_versions
  tunnel2_phase1_dh_group_numbers      = local.ipsec.dh_groups
  tunnel2_phase1_encryption_algorithms = local.ipsec.encryption
  tunnel2_phase1_integrity_algorithms  = local.ipsec.integrity
  tunnel2_phase1_lifetime_seconds      = local.ipsec.phase1_lifetime
  tunnel2_phase2_dh_group_numbers      = local.ipsec.dh_groups
  tunnel2_phase2_encryption_algorithms = local.ipsec.encryption
  tunnel2_phase2_integrity_algorithms  = local.ipsec.integrity
  tunnel2_phase2_lifetime_seconds      = local.ipsec.phase2_lifetime

  depends_on = [terraform_data.preshared_key_check]
}

# To Google: one customer gateway for each Google interface and a connection to each, so four
# tunnels. Connection N carries tunnels 2N and 2N+1.
resource "aws_customer_gateway" "gcp" {
  count = 2

  bgp_asn    = local.asn.gcp
  ip_address = google_compute_ha_vpn_gateway.this.vpn_interfaces[count.index].ip_address
  type       = "ipsec.1"
  tags       = merge(local.tags, { Name = "${var.name}-gcp-${count.index}" })
}

resource "aws_vpn_connection" "gcp" {
  count = 2

  vpn_gateway_id      = aws_vpn_gateway.this.id
  customer_gateway_id = aws_customer_gateway.gcp[count.index].id
  type                = "ipsec.1"
  static_routes_only  = false
  tags                = merge(local.tags, { Name = "${var.name}-gcp-${count.index}" })

  tunnel1_inside_cidr                  = local.tunnels["aws-gcp"][2 * count.index]
  tunnel1_preshared_key                = var.preshared_keys["aws-gcp-${2 * count.index}"]
  tunnel1_ike_versions                 = local.ipsec.ike_versions
  tunnel1_phase1_dh_group_numbers      = local.ipsec.dh_groups
  tunnel1_phase1_encryption_algorithms = local.ipsec.encryption
  tunnel1_phase1_integrity_algorithms  = local.ipsec.integrity
  tunnel1_phase1_lifetime_seconds      = local.ipsec.phase1_lifetime
  tunnel1_phase2_dh_group_numbers      = local.ipsec.dh_groups
  tunnel1_phase2_encryption_algorithms = local.ipsec.encryption
  tunnel1_phase2_integrity_algorithms  = local.ipsec.integrity
  tunnel1_phase2_lifetime_seconds      = local.ipsec.phase2_lifetime

  tunnel2_inside_cidr                  = local.tunnels["aws-gcp"][2 * count.index + 1]
  tunnel2_preshared_key                = var.preshared_keys["aws-gcp-${2 * count.index + 1}"]
  tunnel2_ike_versions                 = local.ipsec.ike_versions
  tunnel2_phase1_dh_group_numbers      = local.ipsec.dh_groups
  tunnel2_phase1_encryption_algorithms = local.ipsec.encryption
  tunnel2_phase1_integrity_algorithms  = local.ipsec.integrity
  tunnel2_phase1_lifetime_seconds      = local.ipsec.phase1_lifetime
  tunnel2_phase2_dh_group_numbers      = local.ipsec.dh_groups
  tunnel2_phase2_encryption_algorithms = local.ipsec.encryption
  tunnel2_phase2_integrity_algorithms  = local.ipsec.integrity
  tunnel2_phase2_lifetime_seconds      = local.ipsec.phase2_lifetime

  depends_on = [terraform_data.preshared_key_check]
}
