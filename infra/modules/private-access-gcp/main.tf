# What a machine with no internet route needs in order to pull its image and use its secret: a
# Private Service Connect endpoint for Google's APIs, which carries the registry and the secret
# store, with private DNS so the usual names resolve to the endpoint's private address inside
# the network. The network has no default route, so nothing here is public.
resource "google_compute_global_address" "apis" {
  name         = "${var.name}-apis"
  purpose      = "PRIVATE_SERVICE_CONNECT"
  address_type = "INTERNAL"
  network      = var.network_id
  address      = "10.255.255.254"
}

resource "google_compute_global_forwarding_rule" "apis" {
  # A Private Service Connect endpoint for Google APIs has a short name of letters and digits.
  name                  = substr("psc${replace(var.name, "-", "")}", 0, 20)
  network               = var.network_id
  ip_address            = google_compute_global_address.apis.id
  target                = "all-apis"
  load_balancing_scheme = ""
}

resource "google_dns_managed_zone" "apis" {
  for_each = {
    googleapis = "googleapis.com."
    registry   = "pkg.dev."
  }

  name        = "${var.name}-${each.key}"
  dns_name    = each.value
  description = "Sends ${each.value} to the private endpoint"
  visibility  = "private"
  labels      = var.labels

  private_visibility_config {
    networks {
      network_url = var.network_id
    }
  }
}

resource "google_dns_record_set" "apis" {
  for_each = google_dns_managed_zone.apis

  managed_zone = each.value.name
  name         = "*.${each.value.dns_name}"
  type         = "A"
  ttl          = 300
  rrdatas      = [google_compute_global_address.apis.address]
}
