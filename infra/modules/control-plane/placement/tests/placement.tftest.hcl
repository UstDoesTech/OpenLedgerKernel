run "five_members_are_placed_two_two_one" {
  command = plan

  assert {
    condition     = var.members == 5 && output.per_cloud == { aws = 2, azure = 2, gcp = 1 }
    error_message = "Five members must be placed two, two and one."
  }

  assert {
    condition     = output.quorum == 3 && output.survives_any_single_loss
    error_message = "Five members must keep a majority when any one cloud is lost."
  }

  assert {
    condition     = output.survivors_after_loss == { aws = 3, azure = 3, gcp = 4 }
    error_message = "Losing a cloud must leave three, three or four members."
  }
}

run "three_members_are_placed_one_each" {
  command = plan

  variables {
    members = 3
  }

  assert {
    condition     = output.per_cloud == { aws = 1, azure = 1, gcp = 1 }
    error_message = "Three members must be placed one in each cloud."
  }

  assert {
    condition     = output.quorum == 2 && output.survives_any_single_loss
    error_message = "Three members must keep a majority when any one cloud is lost."
  }
}

run "six_and_seven_members_survive_the_loss_of_any_cloud" {
  command = plan

  variables {
    members = 7
  }

  assert {
    condition     = output.per_cloud == { aws = 3, azure = 2, gcp = 2 } && output.survives_any_single_loss
    error_message = "Seven members must be placed three, two and two, and keep a majority."
  }
}

run "members_in_a_cloud_use_different_zones" {
  command = plan

  assert {
    condition     = length(distinct([for m in output.members : "${m.cloud}-${m.zone}-${m.slot}"])) == 5
    error_message = "No two members may share a cloud, zone and slot."
  }
}

run "fewer_than_three_members_are_refused" {
  command = plan

  variables {
    members = 2
  }

  expect_failures = [var.members]
}

run "one_member_is_refused" {
  command = plan

  variables {
    members = 1
  }

  expect_failures = [var.members]
}

run "four_members_are_refused_because_losing_a_cloud_would_leave_no_majority" {
  command = plan

  variables {
    members = 4
  }

  expect_failures = [var.members]
}

run "a_fractional_member_count_is_refused" {
  command = plan

  variables {
    members = 3.5
  }

  expect_failures = [var.members]
}

run "one_cloud_uses_its_three_zones_as_the_failure_domains" {
  command = plan

  variables {
    members = 3
    clouds  = ["aws"]
  }

  assert {
    condition     = output.per_domain == { "zone-1" = 1, "zone-2" = 1, "zone-3" = 1 } && output.per_cloud == { aws = 3, azure = 0, gcp = 0 }
    error_message = "With one cloud, three members must be placed one in each zone."
  }

  assert {
    condition     = output.survives_any_single_loss
    error_message = "Losing one zone must leave a majority."
  }
}

run "two_clouds_are_refused" {
  command = plan

  variables {
    clouds = ["aws", "azure"]
  }

  expect_failures = [var.clouds]
}

run "an_unknown_cloud_is_refused" {
  command = plan

  variables {
    clouds = ["aws", "azure", "oracle"]
  }

  expect_failures = [var.clouds]
}

run "members_take_fixed_addresses_in_their_zones" {
  command = plan

  variables {
    cidrs = {
      aws   = ["10.10.16.0/20", "10.10.32.0/20", "10.10.48.0/20"]
      azure = ["10.20.16.0/20", "10.20.32.0/20", "10.20.48.0/20"]
      gcp   = ["10.30.16.0/20", "10.30.32.0/20", "10.30.48.0/20"]
    }
  }

  assert {
    condition = [for m in output.placed : m.address] == [
      "10.10.16.10", "10.20.16.10", "10.30.16.10", "10.10.32.10", "10.20.32.10",
    ]
    error_message = "Members must take host 10 in their zone's range, in turn."
  }

  assert {
    condition     = output.initial_cluster == "ledger-cp-m1=https://10.10.16.10:2380,ledger-cp-m2=https://10.20.16.10:2380,ledger-cp-m3=https://10.30.16.10:2380,ledger-cp-m4=https://10.10.32.10:2380,ledger-cp-m5=https://10.20.32.10:2380"
    error_message = "The initial cluster must name every member by its full name and peer address."
  }

  assert {
    condition     = length(distinct([for m in output.placed : m.address])) == 5
    error_message = "No two members may share an address."
  }
}

run "nine_members_never_share_an_address" {
  command = plan

  variables {
    members = 9
    cidrs = {
      aws   = ["10.10.16.0/20", "10.10.32.0/20", "10.10.48.0/20"]
      azure = ["10.20.16.0/20", "10.20.32.0/20", "10.20.48.0/20"]
      gcp   = ["10.30.16.0/20", "10.30.32.0/20", "10.30.48.0/20"]
    }
  }

  assert {
    condition     = length(distinct([for m in output.placed : m.address])) == 9 && output.survives_any_single_loss
    error_message = "Nine members must have nine addresses and keep a majority."
  }
}

run "one_cloud_spreads_members_over_its_zones" {
  command = plan

  variables {
    members = 5
    clouds  = ["azure"]
    cidrs = {
      azure = ["10.20.16.0/20", "10.20.32.0/20", "10.20.48.0/20"]
    }
  }

  assert {
    condition     = [for m in output.placed : m.address] == ["10.20.16.10", "10.20.32.10", "10.20.48.10", "10.20.16.11", "10.20.32.11"]
    error_message = "With one cloud, members must go round the zones and take the next host in a zone that already has one."
  }

  assert {
    condition     = output.per_domain == { "zone-1" = 2, "zone-2" = 2, "zone-3" = 1 } && output.survives_any_single_loss
    error_message = "Five members in one cloud must be placed two, two and one over its zones."
  }
}

run "a_range_list_of_the_wrong_length_is_refused" {
  command = plan

  variables {
    cidrs = {
      aws = ["10.10.16.0/20"]
    }
  }

  expect_failures = [var.cidrs]
}
