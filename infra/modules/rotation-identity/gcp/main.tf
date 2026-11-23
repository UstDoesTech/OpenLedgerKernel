# The service account the rotate-credentials workflow acts as in Google Cloud. Only that GitHub
# environment can act as it, and its roles are the ones infra/rotation-identity.json lists, on the
# cell's API secret only. No role is given on the project.
locals {
  gcp = jsondecode(file("${path.module}/../../../rotation-identity.json")).gcp
}

data "google_project" "this" {}

resource "google_service_account" "this" {
  account_id   = var.name
  display_name = "Rotates the ledger's certificates and cell credentials"
}

resource "google_service_account_iam_member" "github" {
  service_account_id = google_service_account.this.name
  role               = "roles/iam.workloadIdentityUser"
  member             = "principalSet://iam.googleapis.com/projects/${data.google_project.this.number}/locations/global/workloadIdentityPools/${var.workload_identity_pool}/attribute.environment/${var.environment}"
}

resource "google_secret_manager_secret_iam_member" "api" {
  for_each = toset(local.gcp.secret_roles)

  secret_id = var.api_secret_id
  role      = each.value
  member    = "serviceAccount:${google_service_account.this.email}"
}

resource "google_secret_manager_secret_iam_member" "administrator" {
  for_each = var.administrator_secret_id == "" ? toset([]) : toset(local.gcp.administrator_secret_roles)

  secret_id = var.administrator_secret_id
  role      = each.value
  member    = "serviceAccount:${google_service_account.this.email}"
}

resource "google_secret_manager_secret_iam_member" "nodes" {
  for_each = { for pair in setproduct(var.node_secret_ids, local.gcp.node_secret_roles) : "${pair[0]}|${pair[1]}" => pair }

  secret_id = each.value[0]
  role      = each.value[1]
  member    = "serviceAccount:${google_service_account.this.email}"
}
