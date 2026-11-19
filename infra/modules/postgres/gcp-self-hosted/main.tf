# Three machines, one per zone, on Flatcar Container Linux. Each runs the node image, given by
# digest, as a container. They have fixed private addresses, no public address, and disks
# encrypted with the tenant's key. They pull the image from a private registry and use Secret
# Manager, both over a private endpoint. The first node starts as the primary and commits when
# one of the other two has the change (ANY 1). Its first start creates the administrator's
# credential and stores it as a version of the secret made here, using the machine's own service
# account, which may add a version to that secret and read it, and nothing else.
# There is no automatic failover in this form.
data "google_compute_zones" "this" {
  region = var.region
}

locals {
  zones     = slice(sort(data.google_compute_zones.this.names), 0, 3)
  addresses = [for cidr in var.subnet_cidrs : cidrhost(cidr, 10)]
  names     = [for i in range(3) : "${var.name}-n${i + 1}"]

  node_config = [for i in range(3) : jsonencode({
    tenant                    = var.tenant
    node                      = local.names[i]
    role                      = i == 0 ? "primary" : "standby"
    postgres_version          = 16
    primary_address           = local.addresses[0]
    synchronous_standby_names = i == 0 ? "ANY 1 (${join(", ", slice(local.names, 1, 3))})" : ""
    secret_id                 = google_secret_manager_secret.administrator.id
    secret_store              = { kind = "gcp_secret_manager" }
  })]

  registry = regex("^projects/(?P<project>[^/]+)/locations/(?P<location>[^/]+)/repositories/(?P<repository>[^/]+)$", var.registry_repository_id)
}

locals {
  tls_files = {
    primary = ["server.crt", "server.key", "ca.crt"]
    standby = ["server.crt", "server.key", "ca.crt", "replication.crt", "replication.key"]
  }
}

module "private_access" {
  source = "../../private-access-gcp"
  count  = var.reuse_private_access ? 0 : 1

  name       = var.name
  network_id = var.network_id
  labels     = var.labels
}

module "boot" {
  source = "../../node-boot"
  count  = 3

  name        = local.names[count.index]
  cloud       = "gcp"
  image       = var.image
  storage_gb  = var.storage_gb
  node_config = local.node_config[count.index]
  tls_secret  = google_secret_manager_secret.tls[count.index].id
  tls_files   = count.index == 0 ? local.tls_files.primary : local.tls_files.standby
}

# Each node has a service account of its own, so that it can read its own certificates and no
# other node's.
resource "google_service_account" "node" {
  count = 3

  account_id   = local.names[count.index]
  display_name = "Postgres node ${local.names[count.index]}"
}

# One secret for each node's certificates and keys, as a JSON object with one entry for each
# file. OpenTofu makes the secret and never a version of it: whoever issues the certificates
# adds the values, so they are not in the state. The node reads its own secret at boot.
resource "google_secret_manager_secret" "tls" {
  count = 3

  secret_id = "${local.names[count.index]}-tls"
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
  count = 3

  secret_id = google_secret_manager_secret.tls[count.index].id
  role      = "roles/secretmanager.secretAccessor"
  member    = "serviceAccount:${google_service_account.node[count.index].email}"
}

resource "google_secret_manager_secret" "administrator" {
  secret_id = "${var.name}-administrator"
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

# The nodes may add a version to this secret and read it, and nothing else.
resource "google_secret_manager_secret_iam_member" "adder" {
  count = 3

  secret_id = google_secret_manager_secret.administrator.id
  role      = "roles/secretmanager.secretVersionAdder"
  member    = "serviceAccount:${google_service_account.node[count.index].email}"
}

resource "google_secret_manager_secret_iam_member" "accessor" {
  count = 3

  secret_id = google_secret_manager_secret.administrator.id
  role      = "roles/secretmanager.secretAccessor"
  member    = "serviceAccount:${google_service_account.node[count.index].email}"
}

# The nodes may read the node image from the registry, and nothing else there.
resource "google_artifact_registry_repository_iam_member" "reader" {
  count = 3

  project    = local.registry.project
  location   = local.registry.location
  repository = local.registry.repository
  role       = "roles/artifactregistry.reader"
  member     = "serviceAccount:${google_service_account.node[count.index].email}"
}

resource "google_compute_disk" "data" {
  count = 3

  name   = "${local.names[count.index]}-data"
  zone   = local.zones[count.index]
  type   = "pd-ssd"
  size   = var.storage_gb
  labels = var.labels

  disk_encryption_key {
    kms_key_self_link = var.kms_key_id
  }
}

resource "google_compute_instance" "node" {
  count = 3

  name         = local.names[count.index]
  zone         = local.zones[count.index]
  machine_type = var.machine_type
  labels       = var.labels

  metadata = {
    user-data              = module.boot[count.index].ignition
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
    source = google_compute_disk.data[count.index].id
  }

  network_interface {
    subnetwork = var.subnet_ids[count.index]
    network_ip = local.addresses[count.index]
  }

  shielded_instance_config {
    enable_secure_boot          = true
    enable_vtpm                 = true
    enable_integrity_monitoring = true
  }

  service_account {
    email  = google_service_account.node[count.index].email
    scopes = ["cloud-platform"]
  }

  lifecycle {
    ignore_changes = [boot_disk[0].initialize_params[0].image, metadata["user-data"]]
  }
}
