# The boot configuration for one Postgres machine on Flatcar Container Linux, in Ignition. The
# same configuration serves all three clouds, which is why it lives here once. Flatcar is a
# minimal container operating system: it holds docker and little else, and is on AWS, Azure and
# Google Cloud. The machine has no route to the internet, so the configuration does only what a
# private network allows:
#
#   - finds and mounts its data disk,
#   - signs in to the private registry with its own cloud identity (no stored credential),
#   - pulls the node image by digest and runs it, locked down the same way the container tests
#     and deploy/compose run it,
#   - and turns off the operating system's own updates, which need the internet.
#
# It writes the node's configuration, which names the cloud secret store and the secret, and
# never a credential. The certificates the node needs are read at every start from a cloud
# secret, named here and never held here, by the machine's own identity, into /etc/ledger/tls.
locals {
  registry = split("/", var.image)[0]

  login_arguments = {
    aws   = "${coalesce(var.aws_region, "unset")} ${local.registry}"
    azure = "${local.registry} ${coalesce(var.azure_tenant_id, "unset")}"
    gcp   = local.registry
  }[var.cloud]

  file_mode = {
    private    = 384
    readable   = 420
    executable = 493
  }

  data_url = { for k, v in {
    config       = var.node_config
    prepare_disk = file("${path.module}/files/prepare-data-disk.sh")
    login        = file("${path.module}/files/registry-login-${var.cloud}.sh")
    fetch_tls    = file("${path.module}/files/fetch-tls-${var.cloud}.sh")
    write_tls    = file("${path.module}/files/write-tls.sh")
  } : k => "data:;base64,${base64encode(v)}" }

  ignition = {
    ignition = { version = "3.4.0" }

    storage = {
      directories = [
        { path = "/etc/ledger", mode = 493 },
        { path = "/etc/ledger/tls", mode = 493 },
        { path = "/opt/ledger", mode = 493 },
      ]

      files = [
        { path = "/etc/ledger/node.json", mode = local.file_mode.readable, contents = { source = local.data_url.config } },
        { path = "/opt/ledger/prepare-data-disk.sh", mode = local.file_mode.executable, contents = { source = local.data_url.prepare_disk } },
        { path = "/opt/ledger/registry-login.sh", mode = local.file_mode.executable, contents = { source = local.data_url.login } },
        { path = "/opt/ledger/fetch-tls.sh", mode = local.file_mode.executable, contents = { source = local.data_url.fetch_tls } },
        { path = "/opt/ledger/write-tls.sh", mode = local.file_mode.executable, contents = { source = local.data_url.write_tls } },
      ]
    }

    systemd = {
      units = [
        {
          name    = "ledger-data-disk.service"
          enabled = true
          contents = templatefile("${path.module}/files/ledger-data-disk.service.tpl", {
            storage_bytes = var.storage_gb * 1073741824
          })
        },
        {
          name    = "ledger-postgres.service"
          enabled = true
          contents = templatefile("${path.module}/files/ledger-postgres.service.tpl", {
            name            = var.name
            image           = var.image
            login_arguments = local.login_arguments
            tls_arguments   = "${var.tls_secret} ${join(" ", var.tls_files)}"
          })
        },
        # The operating system's own updates need the internet. A node image is updated by
        # replacing the machine, one at a time, on purpose.
        { name = "update-engine.service", mask = true },
        { name = "locksmithd.service", mask = true },
      ]
    }
  }
}
