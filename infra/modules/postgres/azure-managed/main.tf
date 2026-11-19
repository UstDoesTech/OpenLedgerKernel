# An Azure Database for PostgreSQL flexible server with zone-redundant high availability: a
# standby in another zone, kept in step by synchronous replication. It has no public access.
# It is reached through a private endpoint in the cell tier's subnet, which makes it reachable
# across the private links. Only Microsoft Entra sign-in is on, so there is no password.
data "azurerm_client_config" "current" {}

resource "azurerm_postgresql_flexible_server" "this" {
  name                          = var.name
  location                      = var.location
  resource_group_name           = var.resource_group_name
  version                       = "16"
  sku_name                      = var.sku_name
  storage_mb                    = var.storage_gb * 1024
  auto_grow_enabled             = true
  zone                          = "1"
  backup_retention_days         = var.backup_retention_days
  geo_redundant_backup_enabled  = false
  public_network_access_enabled = false
  tags                          = var.tags

  high_availability {
    mode                      = "ZoneRedundant"
    standby_availability_zone = "2"
  }

  authentication {
    active_directory_auth_enabled = true
    password_auth_enabled         = false
    tenant_id                     = data.azurerm_client_config.current.tenant_id
  }

  identity {
    type         = "UserAssigned"
    identity_ids = [var.identity_id]
  }

  customer_managed_key {
    key_vault_key_id                  = var.key_vault_key_id
    primary_user_assigned_identity_id = var.identity_id
  }

  lifecycle {
    # After a failover the zones swap; do not swap them back.
    ignore_changes = [zone, high_availability[0].standby_availability_zone]
  }
}

resource "azurerm_postgresql_flexible_server_active_directory_administrator" "this" {
  server_name         = azurerm_postgresql_flexible_server.this.name
  resource_group_name = var.resource_group_name
  tenant_id           = data.azurerm_client_config.current.tenant_id
  object_id           = var.administrator.object_id
  principal_name      = var.administrator.principal_name
  principal_type      = var.administrator.principal_type
}

resource "azurerm_private_endpoint" "this" {
  name                = "${var.name}-postgres"
  location            = var.location
  resource_group_name = var.resource_group_name
  subnet_id           = var.subnet_id
  tags                = var.tags

  private_service_connection {
    name                           = var.name
    private_connection_resource_id = azurerm_postgresql_flexible_server.this.id
    subresource_names              = ["postgresqlServer"]
    is_manual_connection           = false
  }

  private_dns_zone_group {
    name                 = "postgres"
    private_dns_zone_ids = [var.private_dns_zone_id]
  }
}
