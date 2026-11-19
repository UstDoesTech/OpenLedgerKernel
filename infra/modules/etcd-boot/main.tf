# The boot configuration for one etcd member on Flatcar Container Linux, in Ignition. It is the
# same as the Postgres node's (modules/node-boot), with etcd's own unit, and it uses the same
# scripts: find and mount the data disk, sign in to the private registry with the machine's own
# identity, read the member's certificates from the cloud's secret store, pull the image by
# digest and run it locked down. The machine has no route to the internet.
#
# Clients and peers both need a certificate from the one authority (mutual TLS), and TLS 1.3 is
# the least that is accepted. The certificates are named here and never held here.
locals {
  registry = split("/", var.image)[0]

  login_arguments = {
    aws   = "${coalesce(var.aws_region, "unset")} ${local.registry}"
    azure = "${local.registry} ${coalesce(var.azure_tenant_id, "unset")}"
    gcp   = local.registry
  }[var.cloud]

  tls_files = ["ca.crt", "server.crt", "server.key", "peer.crt", "peer.key"]

  etcd_arguments = join(" ", [
    "--name ${var.name}",
    "--data-dir /var/lib/etcd",
    "--listen-client-urls https://0.0.0.0:2379",
    "--advertise-client-urls https://${var.address}:2379",
    "--listen-peer-urls https://0.0.0.0:2380",
    "--initial-advertise-peer-urls https://${var.address}:2380",
    "--initial-cluster ${var.initial_cluster}",
    "--initial-cluster-state new",
    "--initial-cluster-token ${var.cluster_id}",
    "--client-cert-auth",
    "--trusted-ca-file /etc/etcd/tls/ca.crt",
    "--cert-file /etc/etcd/tls/server.crt",
    "--key-file /etc/etcd/tls/server.key",
    "--peer-client-cert-auth",
    "--peer-trusted-ca-file /etc/etcd/tls/ca.crt",
    "--peer-cert-file /etc/etcd/tls/peer.crt",
    "--peer-key-file /etc/etcd/tls/peer.key",
    "--tls-min-version TLS1.3",
    "--auto-compaction-mode periodic",
    "--auto-compaction-retention 1h",
  ])

  mode = {
    readable   = 420
    executable = 493
  }

  files = "${path.module}/../node-boot/files"

  data_url = { for k, v in {
    prepare_disk = file("${local.files}/prepare-data-disk.sh")
    login        = file("${local.files}/registry-login-${var.cloud}.sh")
    fetch_tls    = file("${local.files}/fetch-tls-${var.cloud}.sh")
    write_tls    = file("${local.files}/write-tls.sh")
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
        { path = "/opt/ledger/prepare-data-disk.sh", mode = local.mode.executable, contents = { source = local.data_url.prepare_disk } },
        { path = "/opt/ledger/registry-login.sh", mode = local.mode.executable, contents = { source = local.data_url.login } },
        { path = "/opt/ledger/fetch-tls.sh", mode = local.mode.executable, contents = { source = local.data_url.fetch_tls } },
        { path = "/opt/ledger/write-tls.sh", mode = local.mode.executable, contents = { source = local.data_url.write_tls } },
      ]
    }

    systemd = {
      units = [
        {
          name    = "ledger-data-disk.service"
          enabled = true
          contents = templatefile("${path.module}/files/etcd-data-disk.service.tpl", {
            storage_bytes = var.storage_gb * 1073741824
          })
        },
        {
          name    = "etcd-member.service"
          enabled = true
          contents = templatefile("${path.module}/files/etcd-member.service.tpl", {
            name            = var.name
            image           = var.image
            login_arguments = local.login_arguments
            tls_arguments   = "${var.tls_reference} ${join(" ", local.tls_files)}"
            etcd_arguments  = local.etcd_arguments
          })
        },
        # The operating system's own updates need the internet. A member is updated by
        # replacing the machine, one at a time, on purpose.
        { name = "update-engine.service", mask = true },
        { name = "locksmithd.service", mask = true },
      ]
    }
  }
}
