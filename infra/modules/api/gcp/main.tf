# One small machine, on Flatcar Container Linux, that runs the API image next to a cell. It has a
# private address and no public one, and a disk encrypted with the tenant's key. Its service
# account may read the cell's one API secret and read the API image, and nothing else.
data "google_compute_zones" "this" {
  region = var.region
}

locals {
  zone     = sort(data.google_compute_zones.this.names)[0]
  registry = regex("^projects/(?P<project>[^/]+)/locations/(?P<location>[^/]+)/repositories/(?P<repository>[^/]+)$", var.registry_repository_id)
}

module "boot" {
  source = "../../api-boot"

  name             = var.name
  cell             = var.cell
  cloud            = "gcp"
  image            = var.image
  secret           = google_secret_manager_secret.api.id
  etcd_endpoints   = var.etcd_endpoints
  migrate_database = var.migrate_database
}

resource "google_service_account" "api" {
  account_id   = var.name
  display_name = "API ${var.name}"
}

# The secret is made without a version. Whoever issues the certificates and tokens adds the
# values, so they are never in OpenTofu's state.
resource "google_secret_manager_secret" "api" {
  secret_id = "${var.name}-api"
  labels    = var.labels

  replication {
    user_managed {
      replicas {
        location = var.region

        customer_managed_encryption {
          kms_key_name = var.kms_key_id
        }
      }
    }
  }
}

resource "google_secret_manager_secret_iam_member" "api" {
  secret_id = google_secret_manager_secret.api.id
  role      = "roles/secretmanager.secretAccessor"
  member    = "serviceAccount:${google_service_account.api.email}"
}

# The machine may read the API image from the registry, and nothing else there.
resource "google_artifact_registry_repository_iam_member" "reader" {
  project    = local.registry.project
  location   = local.registry.location
  repository = local.registry.repository
  role       = "roles/artifactregistry.reader"
  member     = "serviceAccount:${google_service_account.api.email}"
}

resource "google_compute_instance" "api" {
  # A change to the machine's shielded settings needs it stopped and started again. Without
  # this the provider refuses the change on a running machine.
  allow_stopping_for_update = true

  name         = var.name
  zone         = local.zone
  machine_type = var.machine_type
  labels       = var.labels

  metadata = {
    user-data              = module.boot.ignition
    block-project-ssh-keys = "true"
  }

  boot_disk {
    kms_key_self_link = var.kms_key_id

    initialize_params {
      image = var.operating_system_image
      type  = "pd-ssd"
    }
  }

  # No access_config block: the machine has no public address.
  network_interface {
    subnetwork = var.subnet_id
  }

  # Flatcar's public image for Google Cloud is not signed for Google's Secure Boot keys. With
  # Secure Boot on, the firmware refuses the disk ("Security Violation") and the machine never
  # starts. The virtual TPM and integrity monitoring stay on. Secure Boot comes back with an
  # image of our own that is signed for it.
  shielded_instance_config {
    enable_secure_boot          = false
    enable_vtpm                 = true
    enable_integrity_monitoring = true
  }

  service_account {
    email  = google_service_account.api.email
    scopes = ["cloud-platform"]
  }

  lifecycle {
    ignore_changes = [boot_disk[0].initialize_params[0].image, metadata["user-data"]]
  }
}
