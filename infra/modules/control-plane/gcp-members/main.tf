# The etcd members that run on Google Cloud, one machine each, on Flatcar Container Linux. Each
# runs etcd as a container from an image given by digest. They have fixed private addresses, no
# public address, and disks encrypted with a key. They pull the image from a private registry and
# read their certificates from Secret Manager, both over a private endpoint. Each member has a
# service account of its own that reads its own certificate secret and nothing else.
data "google_compute_zones" "this" {
  region = var.region
}

locals {
  zones    = slice(sort(data.google_compute_zones.this.names), 0, 3)
  members  = { for m in var.members : m.name => m }
  registry = regex("^projects/(?P<project>[^/]+)/locations/(?P<location>[^/]+)/repositories/(?P<repository>[^/]+)$", var.registry_repository_id)
}

module "boot" {
  source   = "../../etcd-boot"
  for_each = local.members

  name            = each.key
  cloud           = "gcp"
  image           = var.image
  storage_gb      = var.storage_gb
  address         = each.value.address
  initial_cluster = var.initial_cluster
  cluster_id      = var.cluster_id
  tls_reference   = google_secret_manager_secret.tls[each.key].id
}

# Each member has a service account of its own, so that it can read its own certificates and no
# other member's.
resource "google_service_account" "member" {
  for_each = local.members

  account_id   = each.key
  display_name = "etcd member ${each.key}"
}

# One secret for each member's certificates and keys, as a JSON object with one entry for each
# file. OpenTofu makes the secret and never a version of it: whoever issues the certificates adds
# the values, so they are not in the state.
resource "google_secret_manager_secret" "tls" {
  for_each = local.members

  secret_id = "${each.key}-tls"
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

resource "google_secret_manager_secret_iam_member" "certificates" {
  for_each = local.members

  secret_id = google_secret_manager_secret.tls[each.key].id
  role      = "roles/secretmanager.secretAccessor"
  member    = "serviceAccount:${google_service_account.member[each.key].email}"
}

# The members may read the etcd image from the registry, and nothing else there.
resource "google_artifact_registry_repository_iam_member" "reader" {
  for_each = local.members

  project    = local.registry.project
  location   = local.registry.location
  repository = local.registry.repository
  role       = "roles/artifactregistry.reader"
  member     = "serviceAccount:${google_service_account.member[each.key].email}"
}

resource "google_compute_disk" "data" {
  for_each = local.members

  name   = "${each.key}-data"
  zone   = local.zones[each.value.zone]
  type   = "pd-ssd"
  size   = var.storage_gb
  labels = var.labels

  disk_encryption_key {
    kms_key_self_link = var.kms_key_id
  }
}

resource "google_compute_instance" "member" {
  for_each = local.members

  name         = each.key
  zone         = local.zones[each.value.zone]
  machine_type = var.machine_type
  labels       = var.labels

  metadata = {
    user-data              = module.boot[each.key].ignition
    block-project-ssh-keys = "true"
  }

  boot_disk {
    kms_key_self_link = var.kms_key_id

    initialize_params {
      image = var.operating_system_image
      type  = "pd-ssd"
    }
  }

  attached_disk {
    source = google_compute_disk.data[each.key].id
  }

  network_interface {
    subnetwork = var.subnet_ids[each.value.zone]
    network_ip = each.value.address
  }

  shielded_instance_config {
    enable_secure_boot          = true
    enable_vtpm                 = true
    enable_integrity_monitoring = true
  }

  service_account {
    email  = google_service_account.member[each.key].email
    scopes = ["cloud-platform"]
  }

  lifecycle {
    ignore_changes = [boot_disk[0].initialize_params[0].image, metadata["user-data"]]
  }
}
