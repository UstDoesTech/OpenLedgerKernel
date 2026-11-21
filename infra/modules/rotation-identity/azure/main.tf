# The identity the rotate-credentials workflow signs in as in Azure. It is a user-assigned managed
# identity with a federated credential, not an app registration: that needs no right in Microsoft
# Entra ID, only the Azure rights the applying identity already holds, and the workflow signs in
# the same way (azure/login with the identity's client id). Only that GitHub environment can use it,
# and its roles are the ones infra/rotation-identity.json lists, on the Key Vault only.
locals {
  azure = jsondecode(file("${path.module}/../../../rotation-identity.json")).azure
}

resource "azurerm_resource_group" "this" {
  name     = "${var.name}-identity"
  location = var.location
}

resource "azurerm_user_assigned_identity" "this" {
  name                = var.name
  location            = azurerm_resource_group.this.location
  resource_group_name = azurerm_resource_group.this.name
}

resource "azurerm_federated_identity_credential" "github" {
  name                = "github-${var.environment}"
  resource_group_name = azurerm_resource_group.this.name
  parent_id           = azurerm_user_assigned_identity.this.id
  audience            = ["api://AzureADTokenExchange"]
  issuer              = "https://token.actions.githubusercontent.com"
  subject             = "${var.subject_prefix}:environment:${var.environment}"
}

resource "azurerm_role_assignment" "vault" {
  for_each = toset(local.azure.vault_roles)

  scope                            = var.vault_id
  role_definition_name             = each.value
  principal_id                     = azurerm_user_assigned_identity.this.principal_id
  principal_type                   = "ServicePrincipal"
  skip_service_principal_aad_check = true
}

resource "azurerm_role_assignment" "administrator_vault" {
  for_each = var.administrator_vault_id == "" ? toset([]) : toset(local.azure.administrator_vault_roles)

  scope                            = var.administrator_vault_id
  role_definition_name             = each.value
  principal_id                     = azurerm_user_assigned_identity.this.principal_id
  principal_type                   = "ServicePrincipal"
  skip_service_principal_aad_check = true
}
