variables {
  name            = "ledger-cp-m1"
  cloud           = "aws"
  image           = "registry.example/ledger/etcd@sha256:0000000000000000000000000000000000000000000000000000000000000000"
  storage_gb      = 50
  address         = "10.10.16.10"
  initial_cluster = "ledger-cp-m1=https://10.10.16.10:2380,ledger-cp-m2=https://10.20.16.10:2380,ledger-cp-m3=https://10.30.16.10:2380"
  cluster_id      = "ledger-cp-cluster"
  tls_reference   = "a-reference-to-a-secret"
  aws_region      = "eu-west-2"
}

run "the_member_runs_etcd_with_mutual_tls_from_the_secret_store" {
  command = plan

  assert {
    condition = alltrue([
      for flag in ["--client-cert-auth", "--peer-client-cert-auth", "--tls-min-version TLS1.3", "--cert-file /etc/etcd/tls/server.crt", "--peer-cert-file /etc/etcd/tls/peer.crt", "--trusted-ca-file /etc/etcd/tls/ca.crt", "--peer-trusted-ca-file /etc/etcd/tls/ca.crt"] :
      strcontains(jsondecode(output.ignition).systemd.units[1].contents, flag)
    ])
    error_message = "etcd must ask clients and peers for certificates and use TLS 1.3 at least."
  }

  assert {
    condition     = strcontains(jsondecode(output.ignition).systemd.units[1].contents, "fetch-tls.sh a-reference-to-a-secret ca.crt server.crt server.key peer.crt peer.key")
    error_message = "The member must read all five files from its secret before it starts."
  }

  assert {
    condition     = !strcontains(jsondecode(output.ignition).systemd.units[1].contents, "http://") && !strcontains(jsondecode(output.ignition).systemd.units[1].contents, "--auto-tls") && !strcontains(jsondecode(output.ignition).systemd.units[1].contents, "--peer-auto-tls")
    error_message = "No plain HTTP and no self-signed certificates."
  }
}

run "the_member_runs_the_image_by_digest_locked_down" {
  command = plan

  assert {
    condition = alltrue([
      for flag in ["--read-only", "--cap-drop ALL", "no-new-privileges", "--user 999:999", "@sha256:0000000000000000000000000000000000000000000000000000000000000000"] :
      strcontains(jsondecode(output.ignition).systemd.units[1].contents, flag)
    ])
    error_message = "The member must run its image by digest, read-only, with no added privileges."
  }

  assert {
    condition     = !strcontains(jsondecode(output.ignition).systemd.units[1].contents, "--privileged") && !strcontains(jsondecode(output.ignition).systemd.units[1].contents, "--network host")
    error_message = "The member must not run privileged or on the host's network."
  }
}

run "the_ignition_holds_no_certificate_and_masks_operating_system_updates" {
  command = plan

  assert {
    condition     = !strcontains(output.ignition, "BEGIN")
    error_message = "No certificate or key may appear in the boot configuration."
  }

  assert {
    condition     = alltrue([for n in ["update-engine.service", "locksmithd.service"] : contains([for u in jsondecode(output.ignition).systemd.units : u.name if try(u.mask, false)], n)])
    error_message = "The operating system's own updates must be off."
  }
}

run "peers_are_the_same_on_every_member" {
  command = plan

  assert {
    condition     = strcontains(jsondecode(output.ignition).systemd.units[1].contents, "--initial-cluster ledger-cp-m1=https://10.10.16.10:2380,ledger-cp-m2=https://10.20.16.10:2380,ledger-cp-m3=https://10.30.16.10:2380")
    error_message = "The initial cluster must name every member."
  }

  assert {
    condition     = strcontains(jsondecode(output.ignition).systemd.units[1].contents, "--advertise-client-urls https://10.10.16.10:2379")
    error_message = "The member must advertise its own private address."
  }
}

run "a_tag_is_refused" {
  command = plan

  variables {
    image = "registry.example/ledger/etcd:3.5"
  }

  expect_failures = [var.image]
}
