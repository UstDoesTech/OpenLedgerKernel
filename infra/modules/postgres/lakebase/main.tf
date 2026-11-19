# A Lakebase Postgres project: PostgreSQL 16 with a protected production branch and a primary
# endpoint that has two standbys in other availability zones, one of them readable. Autosuspend
# is off, so the database is always up. Private Link and the customer-managed key belong to the
# workspace, not the project, so the caller attests to them (see the facade's preconditions).
resource "databricks_postgres_project" "this" {
  project_id = var.name

  spec = {
    pg_version   = 16
    display_name = var.name
  }
}

resource "databricks_postgres_branch" "production" {
  branch_id = "production"
  parent    = databricks_postgres_project.this.name

  spec = {
    no_expiry    = true
    is_protected = var.deletion_protection
  }
}

resource "databricks_postgres_endpoint" "primary" {
  endpoint_id = "primary"
  parent      = databricks_postgres_branch.production.name

  spec = {
    endpoint_type            = "ENDPOINT_TYPE_READ_WRITE"
    autoscaling_limit_min_cu = var.compute_units
    autoscaling_limit_max_cu = var.compute_units
    no_suspension            = true

    group = {
      min                         = 3
      max                         = 3
      enable_readable_secondaries = true
    }
  }
}
