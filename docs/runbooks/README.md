# Runbooks

Step-by-step guides for running the infrastructure from this repository. They come from the first real runs, so each one names a problem that happened and what fixed it.

| Runbook | Use it when |
| --- | --- |
| [First run](first-run.md) | You are setting up a new environment, or a new person is taking over |
| [Troubleshooting plans and applies](troubleshooting.md) | A run failed, or a plan shows something you did not expect |
| [First entry](first-entry.md) | The cells are applied and you want to make the first entry through the API |
| [Rolling a new node image](rolling-the-node-image.md) | `infra/images.json` has a new digest and the running machines still use the old one |
| [Rolling a new API image](rolling-the-api-image.md) | You have built a new API image, or changed an API machine's boot configuration, and the running machines still use the old one |
| [Putting the AWS API and page on the internet](exposing-the-api-in-aws.md) | You want to reach the page and the API of the AWS cell from a browser without a VPN, or you want to take that away again |
| [Pausing for cost](pausing-for-cost.md) | You want the proving environment to stop costing money for a while |
| [Rotating certificates and credentials](rotating-credentials.md) | You are issuing the API machines' certificates and cell credentials, or renewing them (every 7 days), or removing a cell |
| [Rotating the VPN pre-shared key](rotating-vpn-key.md) | A key was shown anywhere it should not have been, or on your own schedule |
| [Credentialed workflows](../credentialed-workflows.md) | You need the settings, trust policies and roles in full |

Two rules hold for all of them:

- Never paste a plan, a log or the output of a command that prints a key into a public place. This repository is public, and so is every workflow log.
- Read a plan before you approve an apply, and apply nothing while a plan is failing.
