# Rolling a new node image onto running machines

Use this when `infra/images.json` holds a new digest and the machines still run the old one.

## Why changing the digest does not roll anything

A digest change plans one update, to `terraform_data.image`, and nothing else. That is correct. Every machine ignores later changes to its boot configuration (`ignore_changes` on `user_data`, `custom_data` or `metadata["user-data"]`), so a running node is never replaced by accident. The new image reaches a machine only when that machine is replaced, one at a time, on purpose. The boot configuration says the same: the operating system's own updates are turned off.

A plan to change a digest that shows machines or disks being **replaced** is wrong. Do not apply it. See [Troubleshooting](troubleshooting.md).

## What the workflow does for you

`infra-apply` has a `replace` input. Give it one to three machines of **one cell**, in the order to roll them, separated by commas. Each is a node or member with its index, such as `module.<name>.aws_instance.node[1]`. (The address is in the plan you read for the layer. It is not secret, but do not paste it into public places.)

- Every machine is planned and fingerprinted before you approve, and you approve the whole roll once.
- Each machine is then replaced in turn: its disk is detached (on AWS the machine is stopped first), the machine is replaced, and the same disk is attached to the new machine. Just before each later machine, the plan is made again and must match the one you saw.
- After each machine, the workflow asks the cloud whether it is running, passes the cloud's own checks (AWS) and has its data disk attached. Then it waits the `settle_minutes` you set (default 10, at most 30) before the next machine.
- It stops at the first failure and says which step. Machines after that one are not touched.

**What it cannot check.** The runners cannot reach the private networks, so the workflow does not know the node's service is running or has caught up with its peers. A longer settle time is not a proof of that. Until the nodes publish their own health, you check the cell yourself between roll runs, and roll **one machine per run** when a cell has anything to lose.

The data disks are their own resource on all three clouds, for the Postgres nodes and the etcd members, so a replaced machine keeps its disk. A cell made before that change still has its old inline AWS disk: see [Moving an AWS data disk out of the machine](troubleshooting.md#moving-an-aws-data-disk-out-of-the-machine) first.

## Rolling a cell

1. Read the plan for the digest change. Expect `terraform_data.image` to update and nothing else. Apply it.
2. Check the whole cell is healthy before you start: all three nodes up, replication caught up, no promotion under way. Stop if it is not.
3. Order the machines. Roll the standbys first and the primary last, and promote a standby before you roll the primary (promotion is manual, with no automatic failover). etcd members are equal: roll one at a time and keep two running.
4. Dispatch `infra-apply` for the layer with `replace` and `settle_minutes`, action `plan`. Read the plan in the plan store. Each machine's counts are in the run summary: one machine and its disk attachment replaced, and no disk deleted.
5. Dispatch it again with action `apply` and approve it.
6. When it finishes, check the cell is healthy and that each node runs the new digest.
7. Plan the layer again and expect `0 to change`.

## If something goes wrong

Stop. The workflow has already stopped. Do not start the next machine by hand. A replaced node that does not recover is one of three, so the cell keeps working while you look. Keep logs out of public places, as everywhere else in these runbooks.
