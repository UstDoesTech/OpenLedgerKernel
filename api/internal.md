# Internal endpoints

These are the endpoints one cell's service offers another, for transfers between cells. They are not the public API and are not in [`openapi.yaml`](openapi.yaml). A cell never connects to another cell's database: it calls these.

## How a call is made

- **Listener.** The internal endpoints are on their own listener (`LEDGER_INTERNAL_LISTEN`), never on the public one. Off loopback it serves mutual TLS only: the caller shows a certificate signed by the same authority as ours, and so do we.
- **Credential.** `Authorization: Bearer <credential>`. The credential names the calling cell and the tenants it may act for. A tenant's own token is never accepted here, and a cell credential is never accepted on the public listener.
- **Tenant.** `x-ledger-tenant: ten_…`. It must be one the credential covers. A missing, malformed or uncovered tenant is `401 unauthenticated`, the same as a wrong credential.
- **Errors.** `application/problem+json` with the same stable codes as the public API. Another tenant's account, and one that does not exist, are the same `404 not_found`.

## Endpoints

### `GET /internal/v1/accounts/{accountId}`

Whether the tenant has this account at this cell. Returns `200` with `account_id` and `currency`, or `404`. The payer's cell uses it to say `wrong_home` only for the caller's own account.

### `POST /internal/v1/transfers/prepare`

The first question of a cross-cell transfer, asked before anything is written at either cell. Body: `journal_id`, `payee_account_id`, `amount` (integer minor units and an ISO 4217 code). It checks that the payee exists for the tenant, is in the amount's currency and is homed here with its lease held, and makes this cell ready to carry the in-transit value. It writes no entry. `200 {"ready": true}` means `receive` can follow.

### `POST /internal/v1/transfers/receive`

The second half. Body: `journal_id`, `payer_account_id`, `payee_account_id`, `amount`. It credits the payee against this cell's in-transit account. Safe to repeat: a repeat writes nothing and says so. Returns `200` with `watermark` and `replayed`.

## The order of a transfer

1. The payer's cell makes its own side of the in-transit pair ready.
2. It calls `prepare`. A decline here, or no answer, stops the transfer with nothing written at either cell.
3. It writes the first half at its own cell.
4. It calls `receive`. If this fails or gets no answer, the caller's retry with the same Idempotency-Key repeats steps 2 to 4, and the transfer finishes exactly once.
5. It marks the journal settled at its own cell.
