# Rolling a new node image onto running machines

Use this when `infra/images.json` holds a new digest and the machines still run the old one.

## Why changing the digest does not roll anything

A digest change plans one update, to `terraform_data.image`, and nothing else. That is correct. Every machine ignores later changes to its boot configuration (`ignore_changes` on `user_data`, `custom_data` or `metadata["user-data"]`), so a running node is never replaced by accident. The new image reaches a machine only when that machine is replaced, one at a time, on purpose. The boot configuration says the same: the operating system's own updates are turned off.

A plan to change a digest that shows machines or disks being **replaced** is wrong. Do not apply it. See [Troubleshooting](troubleshooting.md).

## What the workflow can replace

`infra-apply` has a `replace` input that takes the address of **one** machine, and the plan check refuses a plan that deletes a data disk or replaces any other machine (see [Credentialed workflows](../credentialed-workflows.md)). An agent must never run `tofu` against a real account, and a person does not need to either.

The data disks are their own resources. On all three clouds, for both the Postgres nodes and the etcd members, replacing a machine keeps its disk. If a cell was created before that change, apply the layer first and read the plan: the old inline AWS disks are destroyed and new volumes created, which is safe only while the cell holds no data worth keeping.

The API machines are rolled the same way, one at a time, but they hold no data. See [Rolling a new API image](rolling-the-api-image.md).

## Replacing a machine

Do one machine at a time, and one cloud at a time.

1. Read the plan for the digest change. Expect `terraform_data.image` to update and nothing else. Apply it.
2. Check the whole cell is healthy before you start: all three nodes up, replication caught up, no promotion under way. Stop if it is not.
3. Replace the first machine only. Read the plan: one machine and its disk attachment replaced, and no disk deleted. The order is: detach the disk (on AWS the machine is stopped first), replace the machine, attach the same disk to the new one. On Google the disk is attached as part of the machine.
4. Wait for the node to start the new image and to catch up. Check its running digest is the new one.
5. Check the cell again, then move to the next machine. Never replace two machines of one cell together.
6. After the last machine, plan the layer again and expect `0 to change`.

## The address to replace

Set the workflow's `env` to the environment (for example `europe`), `layer` to the layer in the table, `action` to `plan` first, and `replace` to one address from the table. Write the address exactly as shown: no quotes, no spaces. The number in square brackets is the machine's index, counted from zero, so `[0]` is the first node and `[1]` the second. Check the plan, then run `apply`.

| Machine | Layer | Address to replace |
| --- | --- | --- |
| A node of the AWS cell | `cells` | `module.cell_aws.module.aws_self_hosted[0].aws_instance.node[0]` |
| A node of the Azure cell | `cells` | `module.cell_azure.module.azure_self_hosted[0].azurerm_linux_virtual_machine.node[0]` |
| A node of the Google cell | `cells` | `module.cell_gcp.module.gcp_self_hosted[0].google_compute_instance.node[0]` |
| A control plane member on AWS | `control-plane` | `module.control_plane.module.aws_members[0].aws_instance.member[0]` |
| A control plane member on Azure | `control-plane` | `module.control_plane.module.azure_members[0].azurerm_linux_virtual_machine.member[0]` |
| A control plane member on Google | `control-plane` | `module.control_plane.module.gcp_members[0].google_compute_instance.member[0]` |

The API machines are replaced the same way, with the addresses in [Rolling a new API image](rolling-the-api-image.md). A cell whose Postgres is managed has no nodes to replace.

If the address is refused, the workflow stops before it plans and prints which rule failed. The address must name one machine with its index, and nothing else.

## If something goes wrong

Stop. Do not replace the next machine. A replaced node that does not recover is one of three, so the cell keeps working while you look. Keep logs out of public places, as everywhere else in these runbooks.
