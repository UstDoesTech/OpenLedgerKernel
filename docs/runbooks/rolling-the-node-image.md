# Rolling a new node image onto running machines

Use this when `infra/images.json` holds a new digest and the machines still run the old one.

## Why changing the digest does not roll anything

A digest change plans one update, to `terraform_data.image`, and nothing else. That is correct. Every machine ignores later changes to its boot configuration (`ignore_changes` on `user_data`, `custom_data` or `metadata["user-data"]`), so a running node is never replaced by accident. The new image reaches a machine only when that machine is replaced, one at a time, on purpose. The boot configuration says the same: the operating system's own updates are turned off.

A plan to change a digest that shows machines or disks being **replaced** is wrong. Do not apply it. See [Troubleshooting](troubleshooting.md).

## Two things that are not ready yet

Read these before you plan a rollout. Neither is a reason to skip a step later in this page.

1. **The workflow cannot replace one machine.** `infra-apply` plans and applies a whole layer. There is no input for a single resource to replace, and an agent must never run `tofu` against a real account. Until an input is added (a change to a credentialed workflow, so it needs the owner's review), a rollout cannot be started from the workflow. Do not work around this by running `tofu apply -replace` from a laptop against shared state without the owner's say.
2. **On AWS the data disk is part of the machine.** The AWS module declares the data volume inline (`ebs_block_device`), so replacing an AWS machine deletes its data disk with it, and the new machine starts with an empty one. Azure and Google keep the data disk as its own resource, and it survives a machine replacement. Nothing in this repository yet shows an AWS node refilling an empty disk from its peers, so treat an AWS replacement as losing that node's copy until a test proves otherwise. The fix is to give the AWS volume its own resource and attachment first, in its own pull request, with a plan that is read line by line, because moving the existing volume must not destroy it.

## When both are fixed

Do one machine at a time, and one cloud at a time.

1. Read the plan for the digest change. Expect `terraform_data.image` to update and nothing else. Apply it.
2. Check the whole cell is healthy before you start: all three nodes up, replication caught up, no promotion under way. Stop if it is not.
3. Replace the first machine only. Read the plan: one machine, and on Azure and Google its data-disk attachment, and no disk deleted.
4. Wait for the node to start the new image and to catch up. Check its running digest is the new one.
5. Check the cell again, then move to the next machine. Never replace two machines of one cell together.
6. After the last machine, plan the layer again and expect `0 to change`.

## If something goes wrong

Stop. Do not replace the next machine. A replaced node that does not recover is one of three, so the cell keeps working while you look. Keep logs out of public places, as everywhere else in these runbooks.
