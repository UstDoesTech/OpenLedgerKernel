# Google Cloud: an HA VPN gateway (two interfaces) and a Cloud Router for BGP. The router's
# default advertisement shares the network's subnets with the other clouds.
locals {
  # The four AWS tunnel endpoints facing Google, in the order of the tunnels: connection N
  # carries tunnels 2N and 2N+1, and its customer gateway is Google interface N.
  aws_gcp_outside = [
    aws_vpn_connection.gcp[0].tunnel1_address,
    aws_vpn_connection.gcp[0].tunnel2_address,
    aws_vpn_connection.gcp[1].tunnel1_address,
    aws_vpn_connection.gcp[1].tunnel2_address,
  ]
}

resource "google_compute_router" "this" {
  name    = "${var.name}-router"
  region  = var.gcp.region
  network = var.gcp.network_name

  bgp {
    asn            = local.asn.gcp
    advertise_mode = "DEFAULT"
  }
}

resource "google_compute_ha_vpn_gateway" "this" {
  name       = "${var.name}-vpn"
  region     = var.gcp.region
  network    = var.gcp.network_name
  stack_type = "IPV4_ONLY"
}

# To AWS: four tunnels. Interface N/2 of the Google gateway meets AWS endpoint N.
resource "google_compute_external_vpn_gateway" "aws" {
  name            = "${var.name}-aws"
  redundancy_type = "FOUR_IPS_REDUNDANCY"

  dynamic "interface" {
    for_each = local.aws_gcp_outside

    content {
      id         = interface.key
      ip_address = interface.value
    }
  }
}

resource "google_compute_vpn_tunnel" "aws" {
  count = 4

  name                            = "${var.name}-aws-${count.index}"
  region                          = var.gcp.region
  vpn_gateway                     = google_compute_ha_vpn_gateway.this.id
  vpn_gateway_interface           = floor(count.index / 2)
  peer_external_gateway           = google_compute_external_vpn_gateway.aws.id
  peer_external_gateway_interface = count.index
  shared_secret                   = var.preshared_keys["aws-gcp-${count.index}"]
  router                          = google_compute_router.this.id
  ike_version                     = 2

  depends_on = [terraform_data.preshared_key_check]
}

resource "google_compute_router_interface" "aws" {
  count = 4

  name       = "${var.name}-aws-${count.index}"
  region     = var.gcp.region
  router     = google_compute_router.this.name
  ip_range   = "${cidrhost(local.tunnels["aws-gcp"][count.index], 2)}/30"
  vpn_tunnel = google_compute_vpn_tunnel.aws[count.index].name
}

resource "google_compute_router_peer" "aws" {
  count = 4

  name                      = "${var.name}-aws-${count.index}"
  region                    = var.gcp.region
  router                    = google_compute_router.this.name
  interface                 = google_compute_router_interface.aws[count.index].name
  peer_ip_address           = cidrhost(local.tunnels["aws-gcp"][count.index], 1)
  peer_asn                  = local.asn.aws
  advertised_route_priority = 100
}

# To Azure: two tunnels, one from each Google interface to the single Azure gateway address.
resource "google_compute_external_vpn_gateway" "azure" {
  name            = "${var.name}-azure"
  redundancy_type = "SINGLE_IP_INTERNALLY_REDUNDANT"

  interface {
    id         = 0
    ip_address = azurerm_public_ip.vpn.ip_address
  }
}

resource "google_compute_vpn_tunnel" "azure" {
  count = 2

  name                            = "${var.name}-azure-${count.index}"
  region                          = var.gcp.region
  vpn_gateway                     = google_compute_ha_vpn_gateway.this.id
  vpn_gateway_interface           = count.index
  peer_external_gateway           = google_compute_external_vpn_gateway.azure.id
  peer_external_gateway_interface = 0
  shared_secret                   = var.preshared_keys["azure-gcp-${count.index}"]
  router                          = google_compute_router.this.id
  ike_version                     = 2

  depends_on = [terraform_data.preshared_key_check]
}

resource "google_compute_router_interface" "azure" {
  count = 2

  name       = "${var.name}-azure-${count.index}"
  region     = var.gcp.region
  router     = google_compute_router.this.name
  ip_range   = "${cidrhost(local.tunnels["azure-gcp"][count.index], 1)}/30"
  vpn_tunnel = google_compute_vpn_tunnel.azure[count.index].name
}

resource "google_compute_router_peer" "azure" {
  count = 2

  name                      = "${var.name}-azure-${count.index}"
  region                    = var.gcp.region
  router                    = google_compute_router.this.name
  interface                 = google_compute_router_interface.azure[count.index].name
  peer_ip_address           = cidrhost(local.tunnels["azure-gcp"][count.index], 2)
  peer_asn                  = local.asn.azure
  advertised_route_priority = 100
}
