# An RDS for PostgreSQL Multi-AZ DB cluster: a writer and two readable standbys in three
# zones. A commit returns once the writer and at least one standby have it, so the loss of a
# zone loses nothing. This is RDS for PostgreSQL, not Aurora, so it is vanilla PostgreSQL 16.
# The administrator's password is created and rotated by RDS in Secrets Manager, encrypted with
# the tenant's key, so it never appears in this configuration or its outputs.
data "aws_subnet" "this" {
  count = 3

  id = var.subnet_ids[count.index]
}

resource "aws_db_subnet_group" "this" {
  name       = var.name
  subnet_ids = var.subnet_ids
  tags       = var.tags
}

# Connections must use TLS.
resource "aws_rds_cluster_parameter_group" "this" {
  name        = var.name
  family      = "postgres16"
  description = "Cluster parameters for ${var.name}"
  tags        = var.tags

  parameter {
    name         = "rds.force_ssl"
    value        = "1"
    apply_method = "immediate"
  }
}

resource "aws_rds_cluster" "this" {
  cluster_identifier = var.name
  engine             = "postgres"
  engine_version     = "16"

  db_cluster_instance_class = var.instance_class
  storage_type              = "io1"
  allocated_storage         = var.storage_gb
  iops                      = var.iops

  availability_zones              = data.aws_subnet.this[*].availability_zone
  db_subnet_group_name            = aws_db_subnet_group.this.name
  db_cluster_parameter_group_name = aws_rds_cluster_parameter_group.this.name
  vpc_security_group_ids          = var.security_group_ids
  publicly_accessible             = false

  master_username               = "ledger_admin"
  manage_master_user_password   = true
  master_user_secret_kms_key_id = var.kms_key_arn

  storage_encrypted                   = true
  kms_key_id                          = var.kms_key_arn
  iam_database_authentication_enabled = true
  enabled_cloudwatch_logs_exports     = ["postgresql"]

  backup_retention_period   = var.backup_retention_days
  copy_tags_to_snapshot     = true
  deletion_protection       = var.deletion_protection
  skip_final_snapshot       = false
  final_snapshot_identifier = "${var.name}-final"
  tags                      = var.tags

  lifecycle {
    ignore_changes = [engine_version]
  }
}
