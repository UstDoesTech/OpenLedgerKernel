# The boot configuration for one API machine on Flatcar Container Linux, in Ignition, for all
# three clouds. The machine has no route to the internet, so it does only what a private network
# allows:
#
#   - signs in to the private registry with its own cloud identity (no stored credential),
#   - reads this cell's one API secret with the same identity, into /etc/ledger-api/secrets,
#   - pulls the API image by digest and runs it read-only with every capability dropped,
#   - and turns off the operating system's own updates, which need the internet.
#
# It writes no credential. The machine's own listeners are TLS: the service ends TLS itself, on
# the public port with the cell's certificate and on the internal port with mutual TLS, and the
# service refuses to start otherwise. There is no load balancer in front of it, so TLS does not
# end anywhere else.
locals {
  registry = split("/", var.image)[0]

  secret_files = [
    "db.conn",
    "tokens.json",
    "internal-auth.json",
    "cells.json",
    "server.crt",
    "server.key",
    "ca.crt",
    "etcd-ca.crt",
    "etcd-client.crt",
    "etcd-client.key",
  ]

  login_arguments = {
    aws   = "${coalesce(var.aws_region, "unset")} ${local.registry}"
    azure = "${local.registry} ${coalesce(var.azure_tenant_id, "unset")}"
    gcp   = local.registry
  }[var.cloud]

  secrets_dir = "/run/secrets/api"

  # How often the machine reads its secret again. A rotation reaches the service within this and
  # the service's own check (30 seconds). The rotation job waits for both before it goes on.
  refresh_minutes = 5

  environment = {
    LEDGER_NODE               = var.cell
    LEDGER_DB_FILE            = "${local.secrets_dir}/db.conn"
    LEDGER_TOKENS_FILE        = "${local.secrets_dir}/tokens.json"
    LEDGER_LISTEN             = "0.0.0.0:${var.public_port}"
    LEDGER_INTERNAL_LISTEN    = "0.0.0.0:${var.internal_port}"
    LEDGER_INTERNAL_AUTH_FILE = "${local.secrets_dir}/internal-auth.json"
    LEDGER_CELLS_FILE         = "${local.secrets_dir}/cells.json"
    LEDGER_TLS_CERT_FILE      = "${local.secrets_dir}/server.crt"
    LEDGER_TLS_KEY_FILE       = "${local.secrets_dir}/server.key"
    LEDGER_TLS_CA_FILE        = "${local.secrets_dir}/ca.crt"
    LEDGER_ETCD_ENDPOINTS     = join(",", var.etcd_endpoints)
    LEDGER_ETCD_PREFIX        = var.etcd_prefix
    LEDGER_ETCD_CA_FILE       = "${local.secrets_dir}/etcd-ca.crt"
    LEDGER_ETCD_CERT_FILE     = "${local.secrets_dir}/etcd-client.crt"
    LEDGER_ETCD_KEY_FILE      = "${local.secrets_dir}/etcd-client.key"
  }

  file_mode = {
    executable = 493
  }

  data_url = { for k, v in {
    login         = file("${path.module}/../node-boot/files/registry-login-${var.cloud}.sh")
    fetch_secrets = file("${path.module}/files/fetch-secrets-${var.cloud}.sh")
    write_secrets = file("${path.module}/files/write-secrets.sh")
  } : k => "data:;base64,${base64encode(v)}" }

  ignition = {
    ignition = { version = "3.4.0" }

    storage = {
      directories = [
        { path = "/etc/ledger-api", mode = 493 },
        { path = "/etc/ledger-api/secrets", mode = 493 },
        { path = "/opt/ledger-api", mode = 493 },
      ]

      files = [
        { path = "/opt/ledger-api/registry-login.sh", mode = local.file_mode.executable, contents = { source = local.data_url.login } },
        { path = "/opt/ledger-api/fetch-secrets.sh", mode = local.file_mode.executable, contents = { source = local.data_url.fetch_secrets } },
        { path = "/opt/ledger-api/write-secrets.sh", mode = local.file_mode.executable, contents = { source = local.data_url.write_secrets } },
      ]
    }

    systemd = {
      units = [
        {
          name    = "ledger-api.service"
          enabled = true
          contents = templatefile("${path.module}/files/ledger-api.service.tpl", {
            name                  = var.name
            image                 = var.image
            login_arguments       = local.login_arguments
            secret_arguments      = "${var.secret} ${join(" ", local.secret_files)}"
            public_port           = var.public_port
            internal_port         = var.internal_port
            environment_arguments = join(" ", [for k, v in local.environment : "--env ${k}=${v}"])
          })
        },
        {
          name = "ledger-api-refresh.service"
          contents = templatefile("${path.module}/files/ledger-api-refresh.service.tpl", {
            secret_arguments = "${var.secret} ${join(" ", local.secret_files)}"
          })
        },
        {
          name    = "ledger-api-refresh.timer"
          enabled = true
          contents = templatefile("${path.module}/files/ledger-api-refresh.timer.tpl", {
            every_minutes = local.refresh_minutes
          })
        },
        # The operating system's own updates need the internet. An API image is updated by
        # replacing the machine, one at a time, on purpose.
        { name = "update-engine.service", mask = true },
        { name = "locksmithd.service", mask = true },
      ]
    }
  }
}
