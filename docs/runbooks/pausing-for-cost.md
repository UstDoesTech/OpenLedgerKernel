# Pausing the proving environment to save money

`scripts/proving_power.sh` lists, pauses and resumes the machines of the control plane (`ledger-cp-m*`) and of the first tenant's cells (`bankofmumanddad-<cloud>-001-n*`), and the tenant's managed databases where the cloud allows it. A person runs it, from a shell signed in to the three clouds. An agent never does.

```
scripts/proving_power.sh status
scripts/proving_power.sh pause          # a dry run: prints what it would do
scripts/proving_power.sh pause --yes
scripts/proving_power.sh resume --yes
scripts/proving_power.sh pause --yes --scope cells   # or control-plane, or all (the default)
```

Without `--yes` it changes nothing. It deletes nothing. It stops AWS instances, deallocates Azure machines (so their compute charge stops) and stops Google Cloud instances, and it stops the AWS cluster and the Cloud SQL instance when the tenant's cells are managed. AWS restarts a stopped database by itself after seven days.

What keeps costing while paused:

- **Disks.** Every machine's disks and a managed database's storage.
- **The VPN gateways and tunnels** between the clouds. These are the larger part of the bill, and only destroying the `interconnect` layer stops them.
- **Private endpoints and DNS zones** from `private-access`, and the load of the Azure Key Vault and registries.
- **An Azure managed database with high availability.** Azure cannot stop it. Only destroying it stops the cost.

Resume the control plane before the cells, and give the five etcd members a few minutes to elect a leader. Destroying layers is a separate step: it goes only through the `infra-apply` workflow, by a person.
