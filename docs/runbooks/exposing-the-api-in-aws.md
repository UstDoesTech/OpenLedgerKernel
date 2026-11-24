# Putting the AWS API and page on the internet, and taking them off

Use this when you want to reach the basic page (`/ui`) and the API of the AWS cell from a browser, without a VPN. It puts an internet-facing load balancer in front of the AWS cell's API machine. Azure and Google Cloud stay private. Decision D-22 holds the reasons.

Every step that changes a cloud account is run by a person, through its workflow. An agent never does.

## What it exposes, and what it costs

- **Exposed:** the page at `/ui`, `/healthz`, and the API routes under `/v1`, on one name you choose, over HTTPS on port 443 only. Port 80 has no listener. Every `/v1` route except sign-in needs a token, and the page manages only the AWS cell it is served by.
- **Who can reach it:** only the ranges you list (`edge_client_cidrs`), or, if you turn on `open_to_internet`, any address. With no ranges and the switch off, nothing can connect.
- **What limits abuse:** an AWS WAF rate limit for each address (300 requests in 5 minutes by default), and the service's own sign-in limits (K-23d, below).
- **Cost:** the load balancer and the web ACL cost money while they exist, whether or not anyone uses them. As a rough guide from AWS's list prices, expect tens of US dollars a month at idle, plus traffic. Check the current prices for your region. The certificate, the internet gateway and the route table are free. [Pausing for cost](pausing-for-cost.md) does not stop them: take it down (the last section) when you do not need it.
- **Not covered:** the load balancer's access logs (they need a bucket), and any protection beyond the rate limit and the sign-in limits. Use the proving environment and the test user only. It holds no real money and no customer data.

## The whole internet, and K-23d

Opening the page to `0.0.0.0/0` was refused until each address had its own sign-in limit. K-23d did that. A lock now covers one address and one username, so nobody can lock a person out from elsewhere, and twenty failures from one address across any usernames lock that address out of sign-in for a minute. The service believes `X-Forwarded-For` only from the load balancer's own subnets (`LEDGER_TRUSTED_PROXIES`).

`edge_client_cidrs` **still refuses** `0.0.0.0/0`, `::/0` and any range wider than /8. Taking the whole internet is a separate switch, `open_to_internet`, so that it is always a deliberate step. Do not turn it on until the machine has been replaced (step 4), or every caller behind the load balancer counts as one address.

## Before you start

- K-23a, K-23b and K-23d are merged, and you are on `main`, as the repository's owner, able to approve the `infra-apply` environment.
- The `network`, `control-plane`, `cell-first` and `api` layers of the environment have been applied, and the AWS cell is healthy: `/healthz` says `status` `ok` from inside the network (see [First entry](first-entry.md)).
- You have the name you will use for the page, and you can make DNS records for it. Anywhere in this runbook, `<page name>` means that name. It is never written in the repository, which is public.

## How the inputs are given

`infra-apply` passes OpenTofu only the repository secrets that start with `TF_VAR_` (see [Credentialed workflows](../credentialed-workflows.md)). GitHub shows the names in capitals, and the workflow lowers the part after the prefix. So every setting below is a repository secret in the environment `infra-apply` uses:

| Secret | Value |
| --- | --- |
| `TF_VAR_edge_enabled` | `true`. Makes the internet gateway and the edge route table. |
| `TF_VAR_front_domain_name` | `<page name>`. Without it there is no load balancer. |
| `TF_VAR_certificate_validated` | Leave it out, or `false`, until step 4. |
| `TF_VAR_edge_client_cidrs` | A JSON list of public ranges, such as `["203.0.113.7/32"]`. Optional. |
| `TF_VAR_open_to_internet` | `true` only in step 6. |

A setting you delete from the secrets goes back to its default on the next apply, and the resources it made are destroyed. That is how you turn it off.

## 1. Make the edge subnets

Dispatch **`infra-apply`** with `env` = `europe`, `layer` = `network` and `action` = `plan`. Read the plan: three new subnets and their security group on AWS, and nothing changed or destroyed. Apply it with `action` = `apply` and approve the `infra-apply` environment.

Optionally plan `interconnect` and `private-access` afterwards and expect no changes. The edge tier exists only on AWS, and neither layer uses it.

## 2. Make the certificate

Add `TF_VAR_edge_enabled` = `true` and `TF_VAR_front_domain_name` = `<page name>`, and `TF_VAR_edge_client_cidrs` if you have ranges. Leave `TF_VAR_certificate_validated` out.

Dispatch **`infra-apply`** with `layer` = `api` and `action` = `plan`. Expect `7 to add, 0 to change, 0 to destroy`: the internet gateway, the edge route table, its route, three subnet associations and the certificate. An API machine shown as changed or replaced is wrong. Do not apply it; see [Troubleshooting](troubleshooting.md). Apply it with `action` = `apply`.

The load balancer is not made yet, because a certificate can only be used once its name is proved.

## 3. Make the validation record

The `api` layer's `front` output holds `certificate_validation_records`: a CNAME name and a value. Read it from the apply's log in the plan store (`aws s3 cp s3://<plan store>/<run id>/apply/ - --recursive`, as in [Credentialed workflows](../credentialed-workflows.md)), or in the AWS console under Certificate Manager. Nothing in it is an account identifier.

In Cloudflare, add a **CNAME** record with that name and value, set to **DNS only** (the grey cloud). Cloudflare adds your zone's name to the end, so enter the name without the zone's own suffix, or you get a doubled name that never validates. Do not keep the trailing dot in the value if Cloudflare complains.

Certificate Manager shows the certificate as **Issued** within a few minutes of the record being visible.

## 4. Replace the machine

The service reads `LEDGER_TRUSTED_PROXIES` when it starts, and a machine reads its boot configuration only when it is made. So the AWS API machine must be **replaced once** to learn which addresses are the load balancer's. Do it before step 7, which opens the page to everyone. It is simplest here, while there is no load balancer, because the plan then shows the machine alone. Since K-23e the workflow also accepts the replacement once the load balancer exists, and the plan then also shows its attachment replaced.

Follow step 7 of [Rolling a new API image](rolling-the-api-image.md) for the AWS cell only. `replace` is `module.api_aws.aws_instance.api`. Plan, read, apply, and check `/healthz` from inside the network, as it says. If you also have a new image to roll, do the earlier steps of that runbook first. Its cell is down for a few minutes while the machine is replaced.

## 5. Make the load balancer

Add `TF_VAR_certificate_validated` = `true`. Dispatch **`infra-apply`** with `layer` = `api` and `action` = `plan`. Expect `0 to change, 0 to destroy` and nine resources to add, plus one for each range in `edge_client_cidrs`: the certificate's validation, the load balancer, its target group and attachment, the one listener on 443, the web ACL and its association, the load balancer's rule out to the API, and the API's rule in from the load balancer. Read it for these things:

- one listener, on port 443, with the `ELBSecurityPolicy-TLS13-1-2-Res-2021-06` policy;
- the security group rules are exactly those, and none is open to `0.0.0.0/0`;
- no API machine is touched.

Apply it with `action` = `apply`. It waits for the certificate, then makes the rest. The apply waits up to 30 minutes for the certificate, and fails if the record in step 3 was wrong.

Read the `front` output again. `load_balancer_dns_name` is the name the load balancer answers on.

## 6. Point the page's name at it

In Cloudflare, add a **CNAME** from `<page name>` to `load_balancer_dns_name`, set to **DNS only** (the grey cloud). Keep it grey: with the orange cloud every request comes from Cloudflare's ranges, the allow-list sees none of your callers, and the sign-in limits count everyone as one address.

## 7. Choose who can reach it

- **Named ranges.** If you have fixed addresses, keep `TF_VAR_edge_client_cidrs` as the list. Add or remove a range, and apply the `api` layer again. A range that is not in the list gets no answer at all.
- **Everyone.** If your address changes, add `TF_VAR_open_to_internet` = `true`, plan the `api` layer (one rule added, from `0.0.0.0/0` on port 443 only), read it, and apply. You can keep the list too, and it has no further effect while the switch is on.

## 8. Check it

From an address that is allowed:

1. `curl -sS -o /dev/null -w '%{http_code}\n' https://<page name>/healthz` says `200`.
2. `https://<page name>/ui` loads, with a certificate from Amazon for `<page name>`. Sign in as the test user, whose password is in [Rotating certificates and credentials](rotating-credentials.md). Opening an account and reading it works.
3. Port 80 does not answer: `curl -sS --max-time 8 http://<page name>/` fails to connect.
4. Optionally, check the sign-in limit is per address: get a password wrong six times from one network (a phone on mobile data), then sign in correctly from another. The second works, and the first is told to wait a minute. Do not do this from a network you need.

From an address that is not allowed, with `open_to_internet` off, the connection times out.

Do not test the rate limit by hammering the page. The WAF blocks the address and keeps blocking it until the window ends.

## If something goes wrong

| What you see | What it means |
| --- | --- |
| The apply in step 5 waits and then fails at the certificate | The validation record is missing, wrong, or proxied. Check the name was not doubled and the record is DNS only. Nothing else was made. Fix the record and apply again. |
| *"The edge tier's internet gateway must exist first"* | `TF_VAR_edge_enabled` is not `true`. Set it and plan again. |
| The plan in step 5 shows the API machine replaced | Stop. Something other than the setting you changed moved. Read [Troubleshooting](troubleshooting.md). |
| The plan for a machine replacement shows the load balancer's attachment replaced | Expected once a load balancer exists. The check lets through that one attachment and nothing else of the load balancer. A plan that shows the listener, the web ACL or the load balancer itself changing is not expected: do not apply it. |
| `502` or `503` from the page | The load balancer sees no healthy API machine. The machine may not be ready, or its secret may lack a file: check `/healthz` from inside the network and see [First entry](first-entry.md#when-a-step-fails). |
| `403` from one address, others work | The WAF's rate limit blocked it. It ends when the window does. |
| Sign-in says too many attempts for everyone, from the start | The service is not told the load balancer's ranges, so every caller counts as one address. The machine has not been replaced since K-23b (step 4). |
| The browser says the certificate is for another name | The page's CNAME (step 6) does not point at the load balancer, or points somewhere else. |

## Taking it off

Take down the least you need to.

- **Close it to everyone but the allow-list:** delete `TF_VAR_open_to_internet`, plan and apply the `api` layer. One rule is destroyed.
- **Stop the load balancer and keep the certificate:** set `TF_VAR_certificate_validated` to `false`, or delete it. Plan the `api` layer, which shows only the load balancer, its target group, listener, web ACL and their rules being destroyed, and apply. The cost stops. The page's CNAME now points at nothing, so delete it in Cloudflare too.
- **Take everything down:** do the step above, then delete `TF_VAR_front_domain_name` and `TF_VAR_edge_client_cidrs`, plan and apply the `api` layer (the certificate is destroyed), then delete `TF_VAR_edge_enabled`, plan and apply it again (the internet gateway and the edge route table are destroyed). Delete the validation record and the page's CNAME in Cloudflare. The edge subnets and their security group stay, with no route to the internet and no rules.

Each plan should show destroys only, and nothing about the API machine. If it shows anything else, do not apply it.

Keep logs out of public places, as everywhere else in these runbooks.
