# A private network in one Google Cloud region: three subnets per tier, one per availability
# zone, no default route to the internet, no external addresses, and flow logs on every
# subnet. Google denies all ingress unless a firewall rule allows it, and later modules add
# the rules they need. Google subnets span zones, so subnet N of a tier is where zone N
# resources go (a, b, c). That keeps addressing the same on all three clouds.
locals {
  subnets = merge([
    for tier, cidrs in var.plan.subnets : {
      for zone, cidr in cidrs : "${tier}-${zone}" => { tier = tier, zone = zone, cidr = cidr }
    }
  ]...)
}

resource "google_compute_network" "this" {
  name                            = var.name
  auto_create_subnetworks         = false
  routing_mode                    = "REGIONAL"
  delete_default_routes_on_create = true
}

resource "google_compute_subnetwork" "this" {
  for_each = local.subnets

  # Google names allow only lower-case letters, digits and hyphens, and a tier is "control_plane".
  name                     = "${var.name}-${replace(each.key, "_", "-")}"
  region                   = var.region
  network                  = google_compute_network.this.id
  ip_cidr_range            = each.value.cidr
  private_ip_google_access = true

  log_config {
    aggregation_interval = "INTERVAL_5_SEC"
    flow_sampling        = 1.0
    metadata             = "INCLUDE_ALL_METADATA"
  }
}
