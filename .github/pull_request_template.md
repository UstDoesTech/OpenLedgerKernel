## What changed

## How you know it works
Tests run, or the plan result (counts only; never paste plan text, which lists identifiers).

## Checklist
- [ ] `make check` passes
- [ ] No identifiers, keys, tokens or personal data in the diff, logs or fixtures
- [ ] Does not weaken append-only entries, epoch fencing, idempotency, tenant isolation or the ledger's lack of personal data
- [ ] Changes to `.github/`, `infra/` or `scripts/` are marked for the owner's review
