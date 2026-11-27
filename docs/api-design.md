# API design

Design document · 13 November 2026 · Ust Oldfield

Related: [Kernel design](design.md) · [OpenAPI spec](../api/openapi.yaml)

One versioned HTTP API serves the customer app, the colleague console, rail adapters and AI agents: every write is a typed command that is simulated, decided by policy and then committed, and every read names how fresh it is. The machine-readable OpenAPI spec lives in the repository at [`api/openapi.yaml`](../api/openapi.yaml).

## Shape of the API

Writes and reads are separate halves: writes go through one command endpoint family that can always simulate first, and reads are plain resources that report their freshness.

| Half | Endpoints | Behaviour |
| --- | --- | --- |
| Commands | `POST /v1/commands/{type}:simulate`, `POST /v1/commands/{type}`, `GET /v1/commands/{id}` | Every state change, from opening an account to promoting a cell. Policy-checked, idempotent, audited |
| Approvals | `GET /v1/approvals`, `POST /v1/approvals/{id}:approve`, `POST /v1/approvals/{id}:reject` | People decide on commands that policy holds back |
| Catalogue | `GET /v1/catalogue/commands`, `GET /v1/catalogue/commands/{type}` | Machine-readable schema, risk class and authority for every command; the source of agent tools |
| Reads | `/v1/accounts`, `/v1/journals`, `/v1/holds`, `/v1/cells`, `/v1/agents`, `/v1/quarantine`, `/v1/audit` | Resources, never written to directly |
| Events | `GET /v1/events` (stream), webhook subscriptions | Ordered changes for screens and agents to react to |

- **One API for everyone.** The customer app, the colleague console, rail adapters, scheduled product jobs and agents call the same endpoints. What differs is the token, and so the scopes and the policy outcome.
- **No resource is written with PUT or PATCH.** Changing an account is a command such as `holds.place` or `accounts.close`, so every change has a name, a reason, a policy decision and a journal.
- **Open banking stays separate.** Third-party providers reach the bank through a standards-compliant gateway that translates their calls into the same commands.

## Identity and authorisation

Every call carries a short-lived token that names the tenant, the kind of principal and who is accountable, and the token's scopes list the command types it may even ask for.

| Principal | Authenticates with | Token carries | Typical scopes |
| --- | --- | --- | --- |
| Customer | App sign-in with strong customer authentication, device-bound | `tenant`, `party_id`, `sca_level`, `device_id` | Own accounts: read, `payments.send`, `transfers.internal`, `cards.freeze` |
| Colleague | Workforce single sign-on with phishing-resistant MFA | `tenant`, `colleague_id`, `roles`, `elevation` (time-bound, per task) | By role; elevated scopes only while an elevation is active |
| Agent | OAuth client credentials over mutual TLS, token bound to the certificate | `tenant`, `agent_id`, `agent_version`, `model_version`, `owner_id` | The agent class's catalogue entries, nothing else |
| Service | Mutual TLS workload identity | `tenant`, `service_id` | Rail adapters: credit and status commands for their own rail |

- **Scope is necessary, never sufficient.** A scope lets a principal submit a command type. The policy engine still decides each instance on amounts, customer, rolling exposure and system state.
- **Tenant comes from the token, never the path or body.** A request that names a resource from another tenant gets `404`, so existence does not leak across tenants.
- **On behalf of.** A colleague or agent acting for a customer names the party in `on_behalf_of`, and the audit trail records both.
- **Tokens last minutes.** Agent tokens are re-issued at most every 15 minutes, so pausing an agent stops it within one token lifetime, and immediately at the policy engine.

### Sign-in with a username and password (basic UI)

Until real sign-in (A-06) replaces it, a person can sign in to a cell with a username and password, so the basic UI has something to use. It is not one of the principals above. It makes the same kind of bearer token, so every rule above still holds.

- `POST /v1/sessions` takes `{username, password}` and needs no token. A correct pair returns `{token, expires_in, principal}`. The token is 64 hexadecimal characters, lasts 30 minutes and is held in the cell's memory as a hash, so a restart ends every sign-in. `POST /v1/sessions:end` ends the token that was sent.
- The users are a list in the cell's API secret, read from `LEDGER_USERS_FILE` and read again when it changes: `{username, password_hash, tenant, principal}`. Passwords are kept only as Argon2id hashes. The tenant and principal come from the list and never from the request, and a username is personal data, so it never reaches a journal, a log or a reply. The `principal` is an opaque name such as `usr_ada`.
- An unknown user and a wrong password get the same `401` (`invalid_credentials`), and an unknown user is checked against a throwaway hash, so the time does not tell them apart. Five failures in a row from one address for one username lock that address out of that username for a minute with `429` (`too_many_attempts`), and a correct password does not lift the lock early. The same username from another address is not locked, so nobody can lock a person out from elsewhere. Twenty failures from one address across any usernames, within a minute, lock that address out of sign-in for a minute with the same `429`, whether or not the username exists. The address is the connection's, or, when the connection comes from a range in `LEDGER_TRUSTED_PROXIES` (the load balancer's subnets), the right-most `X-Forwarded-For` entry that is not itself in such a range. With none set, the header is never read. An address is counted in memory only. It is not logged or returned. A cell with no `LEDGER_USERS_FILE` answers `404` (`sign_in_not_available`).
- At most four hashes run at once, so a flood of sign-ins cannot starve the ledger.

### How the service signs in to its own database

A self-hosted cell gives the service a role with a password in `db.conn`. A managed cell (RDS, an Azure flexible server, Cloud SQL, Lakebase) does not (D-24). Its role belongs to an identity that the cell's cloud or Databricks assigns to the database role, and the service signs in as that identity.

- `LEDGER_DB_SIGN_IN` is `password` (the default, so nothing changes for a cell that is already applied), `aws_iam`, `azure_entra`, `gcp_iam` or `lakebase_oauth`. Any other word stops the service at start. With an identity setting, `db.conn` holds a host, a user and a database and no password, and a `db.conn` that holds a password stops the service at start.
- The word comes from `LEDGER_DB_SIGN_IN` when that is set. Otherwise it comes from the file `LEDGER_DB_SIGN_IN_FILE` names, which is the secret's `db-sign-in` entry, written by rotate-credentials for a managed cell only. With neither, or a file that is missing or empty, it is `password`, so a cell that is applied does not change. A word in the file goes through the same check as the variable, so an unknown word stops the service and is never read as a password.
- Each time the service connects, it asks its token source for a short-lived token and uses it as the password for that one connection. AWS signs an RDS token with the machine's role. Azure and Google Cloud ask the machine's metadata service for an access token. Lakebase trades a Databricks service principal's secret for a workspace token and then for a database credential, because a service principal has no machine identity to read. The source for Lakebase reads that secret from `LEDGER_DB_OAUTH_FILE` each time it connects.
- The database checks a token only when a connection is made. A connection that is open keeps working after its token has expired, so the service asks for no token while a connection is open.
- A connection that is lost is made again at once with a new token, and the requests that arrive meanwhile fail as they do for any lost connection. If a new connection cannot be made within the length of the lease, the service says so in one line and stops, because it could no longer serve honestly.
- No token, secret, host, role or reply from an identity service is logged, returned or printed. A failure is one of three fixed phrases: the service did not answer, it refused this machine's identity, or its answer was not a token. Plain HTTP goes only to this machine and to the cloud metadata addresses.

### How a cell gets its ledger schema

A cell's database is built empty. The service checks at start that the schema is there at the version it needs, and stops with "migrate it first" if it is not. It never changes the schema itself (K-25).

- `ledger-api migrate` applies each missing migration in order, each in its own transaction, using the connection the service already has. It can be run again and changes nothing the second time. A migration that fails leaves the schema at its last good version and the command exits with an error.
- It prints one line with the version and counts, and never a host, a role or a password.
- A setting in the infrastructure, off by default, adds a one-off unit to the API machine that runs the command before the API starts, and the API will not start if the unit fails. It is switched on for as long as it takes and then off. See [Applying the ledger schema](runbooks/applying-the-ledger-schema.md).

## Command lifecycle

A command is simulated against the home cell's current state, decided by policy, approved by a person when policy requires it, and committed only if its effect still matches the hash that was simulated and approved.

![Command lifecycle: the caller simulates through the command API, policy decides, the home cell dry-runs; an approver approves the exact effect when policy requires it; the commit rechecks the hash at the home cell](images/command-lifecycle.png)

If the account's state changed between simulation and commit, for example another payment landed, the home cell refuses with `effect_changed` and the caller simulates again. An approval is therefore always for the postings that are written, never a stale version of them. Commands policy allows outright can skip the separate simulate call: `POST /v1/commands/{type}` simulates and commits in one step, and still returns the effect and its hash.

## Command envelope and results

Every command, whatever its type, uses one envelope and returns one result shape, so a client or agent handles a fee refund and a cell promotion the same way.

**Request**

| Field | Required | Meaning |
| --- | --- | --- |
| `Idempotency-Key` header | Yes | Client-chosen key; a repeat with the same body returns the first result |
| `payload` | Yes | The command's own fields, validated against its catalogue schema |
| `reason` | Colleagues and agents | Why, in plain words; stored with the journal |
| `evidence` | No | References to records that support the command: conversation, file line, alert |
| `on_behalf_of` | When acting for a customer | The party id |
| `simulation_id` and `effect_hash` | On commit after simulate | Binds the commit to the simulated effect; a mismatch returns `409 effect_changed` |

**Result**

| Field | Meaning |
| --- | --- |
| `command_id`, `status` | `simulated`, `committed`, `pending_approval`, `accepted`, `denied`, `rejected` or `failed` |
| `effect` | Journals, entries, holds and balance changes, exactly as they would be or were written |
| `effect_hash` | Hash of the effect, for approval and commit |
| `decision` | Policy outcome, policy version, rules that fired, and the approval route if any |
| `durability` | `standard` or `strong`, and the clouds that acknowledged |
| `watermark` | The (cell, epoch, sequence) the command committed at, for read-your-writes |

`accepted` means the intent is durable but the home cell is failing over; the command completes when the new home is live and the client polls or follows events.

**Example: an agent refunds a fee**

```json
POST /v1/commands/fees.refund:simulate
Idempotency-Key: 6c0e2f6a-servicing-acc_4d82c6-jr_7b21

{
  "payload": {
    "account_id": "acc_4d82c6",
    "original_journal_id": "jr_7b2190",
    "amount": { "minor": 500, "currency": "GBP" }
  },
  "reason": "Fee charged on a payment the scheme returned in error",
  "evidence": ["conv_91aa04", "pay_3d0e11"],
  "on_behalf_of": "pty_41b8e0"
}

200 OK
{
  "command_id": "cmd_c41f07",
  "status": "simulated",
  "simulation_id": "sim_5e93b2",
  "effect": {
    "journals": [{
      "journal_id": "jr_8d10a2",
      "entries": [
        { "account_id": "gl_fee_income", "side": "debit", "amount": { "minor": 500, "currency": "GBP" } },
        { "account_id": "acc_4d82c6", "side": "credit", "amount": { "minor": 500, "currency": "GBP" } }
      ]
    }],
    "balances": [{ "account_id": "acc_4d82c6", "available_after": { "minor": 116220, "currency": "GBP" } }]
  },
  "effect_hash": "sha256:2f7c...91ab",
  "decision": { "outcome": "allow", "policy": "fees.refund v7", "rules": ["agent_limit_25gbp"] },
  "durability": { "tier": "standard" }
}

POST /v1/commands/fees.refund
Idempotency-Key: 6c0e2f6a-servicing-acc_4d82c6-jr_7b21

{ "simulation_id": "sim_5e93b2", "effect_hash": "sha256:2f7c...91ab" }

201 Created
{ "command_id": "cmd_c41f07", "status": "committed", "watermark": { "cell": "aws-lon-03", "epoch": 5, "seq": 214 } }
```

## Command catalogue

The catalogue is the contract: each command type publishes its schema, effect on the ledger, reversal, risk class and the default authority each principal kind gets, and tenants can only tighten those defaults.

| Command | Effect | Reversal | Agents by default | Customers |
| --- | --- | --- | --- | --- |
| `accounts.open` | Ledger accounts for a product instance | `accounts.close` | Act within limits, after KYC passes | Yes |
| `accounts.close` | Closes once balance and holds are zero | Reopen within 30 days | Act within limits; financial-crime closures are human only | Yes |
| `holds.place`, `holds.release` | Hold on available balance | Each other | Act within limits | No |
| `payments.send` | Hold, scheme message, journal on acceptance | Recall request only | Act within limits for repairs and re-sends | Yes |
| `transfers.internal` | Journal between accounts, via in-transit across cells | Reverse journal | Act within limits | Own accounts |
| `fees.charge`, `fees.refund` | Journal against fee income | Each other | Refund within limits | No |
| `cards.freeze`, `cards.unfreeze` | Card status at the processor | Each other | Act within limits | Yes |
| `plans.create` | Payment plan schedule, interest and fee pause | `plans.cancel` | Inside policy; outside it needs approval | No |
| `quarantine.repost` | Re-posts a voided entry under the current epoch | Reverse journal | Act with approval | No |
| `app_claims.decide` | Reimbursement journal or decline | Reimbursement can be reversed on appeal | Prepare only; decision is human | No |
| `cells.drain` | Routes new requests away from a cell | `cells.undrain` | Act within limits | No |
| `cells.promote` | New epoch and home for a cell's accounts | Promote back | Act with approval | No |
| `agents.pause`, `agents.resume` | Agent class stops or starts | Each other | Pause only, and only for itself or during an incident | No |
| `products.release`, `policy.change`, `permissions.change` | New version of a definition or rule | Release the previous version | Never | No |

Each entry also states its durability rule, for example `payments.send` is strong above the tenant's threshold and always for CHAPS.

## Reads

Reads are served by any region by default and always say how fresh they are; a caller that needs the authoritative answer asks for the home, or waits for its own write's watermark.

| Resource | Notes |
| --- | --- |
| `GET /v1/accounts/{id}` | Product, home cell, epoch, standby, status; personal data never included |
| `GET /v1/accounts/{id}/balances` | Posted, held, available, arranged overdraft, accrued interest |
| `GET /v1/accounts/{id}/entries` | In (epoch, sequence) order; filter by posting time or value date |
| `GET /v1/accounts/{id}/holds` | Active and recent holds |
| `GET /v1/journals/{id}` | Entries, command, actor, reason, evidence, policy decision, replication acknowledgements |
| `GET /v1/parties/{id}` | Masked by default; `?reveal=true` needs a scope and a `reason`, and is logged |
| `GET /v1/cells`, `GET /v1/cells/{id}` | Role, epoch, lease, standby position, lag, drills |
| `GET /v1/agents`, `GET /v1/agents/{id}` | Version, owner, authority, limits, status, rolling exposure |
| `GET /v1/quarantine`, `GET /v1/reconciliation/breaks` | Open items with owner and age |
| `GET /v1/audit` | Every command and read of personal data, filterable by actor, account or time |

**Freshness**

- Every read response carries `Ledger-Watermark: aws-lon-03;5;214` for the account's home position it reflects, and `Ledger-As-Of` with the time.
- `?consistency=home` routes the read to the account's home cell. Balance checks before a payment, and agents deciding whether to act, use it.
- `?min_watermark=aws-lon-03;5;214` waits up to two seconds for a replica to catch up, then falls back to the home. Screens use it right after their own write.

**Pagination**

Lists use opaque cursors, never offsets, because new entries arrive constantly. Entry lists page by (epoch, sequence), so a page never skips or repeats an entry across a failover.

## Events

Every committed command, approval decision and system change becomes an event, delivered in order per account or cell, so screens and agents react to changes instead of polling.

- **Stream.** `GET /v1/events?types=journal.posted,approval.requested&cursor=…` holds a server-sent events connection. Each event has an id that doubles as a resume cursor.
- **Webhooks.** Tenants and internal services subscribe with `POST /v1/subscriptions`. Deliveries are signed, retried with back-off, and carry the same ids, so a receiver deduplicates by id.
- **Order.** Events for one account arrive in (epoch, sequence) order. Across accounts there is no global order, and consumers must not assume one.
- **Content.** Events carry identifiers, amounts and statuses, never personal data. A consumer that needs more reads the resource, which applies masking and logging.

| Event | Raised when |
| --- | --- |
| `journal.posted` | A journal commits at its home |
| `hold.placed`, `hold.released` | Holds change |
| `command.pending_approval`, `approval.decided` | A command waits for a person, and when they decide |
| `payment.status_changed` | A scheme accepts, rejects or returns a payment |
| `quarantine.opened`, `quarantine.resolved` | Entries are voided after a failover, and when they are re-posted |
| `cell.state_changed`, `cell.promoted` | Draining, lease loss, promotion |
| `agent.paused`, `agent.breaker_tripped` | An agent stops, by command or by its circuit breaker |
| `invariant.breached` | Any ledger invariant fails |

## Agent interface

Agents use the same HTTP API through an MCP server generated from the catalogue, so an agent's tools are exactly its scopes, and nothing an agent can call bypasses the policy engine.

**Tools generated per agent class**

| Tool | Maps to | Notes |
| --- | --- | --- |
| `simulate_<command>`, one per scoped command | `POST /v1/commands/{type}:simulate` | Description includes the agent's current limits for that command |
| `commit` | `POST /v1/commands/{type}` with `simulation_id` and `effect_hash` | Only commits a simulation this agent made; returns `pending_approval` when policy holds it |
| `get_<resource>`, `list_<resource>` | Read endpoints | Masked by default; reveal needs scope and reason |
| `my_limits` | `GET /v1/agents/me/exposure` | What the agent has used of each limit in the current windows |
| `hand_off` | `POST /v1/handoffs` | Moves an item to a human queue with a summary and evidence |
| `wait_for_events` | `GET /v1/events` | Filtered to the agent's own commands and queues |

Approval tools are never generated for agents, so an agent cannot approve anything, including its own proposals.

**Guardrails the API enforces**

- **Untrusted text is labelled.** Any field that holds customer-written or third-party text, such as payment references, chat messages or document contents, comes back inside an `untrusted` object. The tool description tells the model that its contents are data, never instructions.
- **Every call names its run.** An `Agent-Run-Id` header ties each tool call to the gateway's record of the prompt, model version and reasoning, so a journal can be traced back to the conversation that caused it.
- **Rate and value limits answer in the response.** A call over a limit gets `429` or `403 limit_exceeded` with the limit, the window and when it resets, so the agent can hand off instead of retrying.
- **The breaker is server-side.** When an agent's error or reversal rate trips its breaker, its token stops being accepted for writes, whatever the agent does.
- **Same envelope, stricter reason.** Agents must send `reason` and at least one `evidence` reference on every write.

## Errors, idempotency and retries

Errors use problem details (`application/problem+json`, RFC 9457) with a stable `code`, and each code says whether a retry with the same idempotency key is safe.

| HTTP | `code` | Meaning | Retry with same key |
| --- | --- | --- | --- |
| 400 | `invalid_payload` | Fails the command's schema | No; fix the payload |
| 401 | `unauthenticated` | No valid bearer token | No; send a valid token |
| 403 | `policy_denied` | Policy refuses this instance | No |
| 403 | `limit_exceeded` | Over a value or rate limit; body gives limit and reset time | After reset, or hand off |
| 404 | `not_found` | Missing, or in another tenant | No |
| 409 | `effect_changed` | The world moved since simulation | No; simulate again |
| 409 | `idempotency_conflict` | Key reused with a different body | No |
| 422 | `insufficient_funds` | Available balance too low | No |
| 421 | `wrong_home` | This cell is not home for the account; `home` names the cell that is | No; send it to the home cell |
| 429 | `rate_limited` | Too many requests | Yes, after `Retry-After` |
| 503 | `home_unavailable` | Home cell failing over and the command cannot be queued | Yes, after `Retry-After` |
| 504 | `durability_not_met` | Committed at home, strong acknowledgement missing | Yes; the retry completes the acknowledgement and never double-posts |

- Idempotency keys are kept for 7 days per tenant and principal. A retry inside that window returns the original result, including the original status code.
- Fencing and home moves are handled inside the API: a client never sees an epoch error, only `accepted`, `home_unavailable` or success.
- Every response carries `traceparent`, so a failed call can be found in traces and in the audit trail.

## Conventions

| Topic | Rule |
| --- | --- |
| Money | `{"minor": 6150, "currency": "GBP"}`: integer minor units and an ISO 4217 code. Never floats, never formatted strings |
| Identifiers | Opaque, prefixed by kind: `acc_`, `jr_`, `cmd_`, `sim_`, `apr_`, `hld_`, `pty_`, `agt_`. Cells use readable names such as `aws-lon-03` |
| Time | RFC 3339 in UTC for instants; plain dates (`2026-11-13`) for value dates |
| Naming | `snake_case` fields; commands as `noun.verb` |
| Versioning | Path version `/v1` for the API; each command type has its own schema version in the catalogue, and changes are additive. A breaking change is a new command type, run side by side until callers move |
| Deprecation | `Deprecation` and `Sunset` headers on old command versions, and a catalogue flag agents read |
| Tenancy | From the token only; never in paths or bodies |
| Caching | Every response from the service says `Cache-Control: no-store`, whatever it holds. A balance, an account and a token are never for a cache to keep. The basic page's own files say the same |
| Personal data | Only in `/v1/parties`, masked by default; never in commands' effects, events or entries |
