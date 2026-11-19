# The cell administrator identity in each cloud. It may administer this tenant's databases and
# has no right to use the tenant's key or any other tenant's cells.
#
# Azure: a person makes the Entra group, and its id comes in as a variable (the outputs hand it
# on). Google Cloud: a service account of the cell's own.
resource "google_service_account" "administrator" {
  account_id   = "${local.name}-admin"
  display_name = "Cell administrator for ${local.tenant}"
}
