# A Cloud SQL for PostgreSQL 16 instance with regional availability: a standby in another
# zone, kept in step by synchronous replication, with automatic failover. It has no public
# address. It is reached through Private Service Connect: an address in the cell tier's subnet
# forwards to the instance, which makes it reachable across the private links. Sign-in is by
# IAM only, so there is no password.
data "google_project" "this" {}

resource "google_sql_database_instance" "this" {
  name                = var.name
  region              = var.region
  database_version    = "POSTGRES_16"
  encryption_key_name = var.kms_key_id
  deletion_protection = var.deletion_protection

  settings {
    tier                        = var.tier
    edition                     = "ENTERPRISE"
    availability_type           = "REGIONAL"
    disk_type                   = "PD_SSD"
    disk_size                   = var.storage_gb
    disk_autoresize             = true
    deletion_protection_enabled = var.deletion_protection
    user_labels                 = var.labels

    ip_configuration {
      ipv4_enabled = false
      ssl_mode     = "ENCRYPTED_ONLY"

      psc_config {
        psc_enabled               = true
        allowed_consumer_projects = [data.google_project.this.project_id]
      }
    }

    backup_configuration {
      enabled                        = true
      point_in_time_recovery_enabled = true
      location                       = var.region
      transaction_log_retention_days = 7

      backup_retention_settings {
        retained_backups = var.backup_retention_days
      }
    }

    database_flags {
      name  = "cloudsql.iam_authentication"
      value = "on"
    }
  }
}

resource "google_compute_address" "psc" {
  name         = "${var.name}-psc"
  region       = var.region
  subnetwork   = var.subnet_id
  address_type = "INTERNAL"
}

resource "google_compute_forwarding_rule" "psc" {
  name                  = "${var.name}-psc"
  region                = var.region
  network               = var.network_id
  ip_address            = google_compute_address.psc.id
  load_balancing_scheme = ""
  target                = google_sql_database_instance.this.psc_service_attachment_link
}

# The instance's own DNS name resolves to the endpoint inside the network.
resource "google_dns_managed_zone" "psc" {
  name       = "${var.name}-psc"
  dns_name   = google_sql_database_instance.this.dns_name
  visibility = "private"
  labels     = var.labels

  private_visibility_config {
    networks {
      network_url = var.network_id
    }
  }
}

resource "google_dns_record_set" "psc" {
  name         = google_sql_database_instance.this.dns_name
  managed_zone = google_dns_managed_zone.psc.name
  type         = "A"
  ttl          = 300
  rrdatas      = [google_compute_address.psc.address]
}

resource "google_sql_user" "administrator" {
  name     = trimsuffix(var.administrator_email, ".gserviceaccount.com")
  instance = google_sql_database_instance.this.name
  type     = "CLOUD_IAM_SERVICE_ACCOUNT"
}
