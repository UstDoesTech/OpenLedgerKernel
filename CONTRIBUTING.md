# Contributing

Run these before you open a pull request: `make check` (formatting, lints and tests), and `make up` first if you want the tests that need the three-node environment.

Rules the kernel keeps, which a change must not weaken: entries are never updated or deleted (corrections are new journals); a stale home cell cannot write after a failover (epoch fencing); every command is idempotent; money is integer minor units plus an ISO 4217 code, never floating point; and no personal data goes in the ledger.

Every behaviour change needs a test. Concurrency and failover behaviour is tested against real Postgres nodes, never mocked. No secrets, tokens or personal data in the repository, logs or fixtures.

By contributing you agree your work is licensed under Apache-2.0, as section 5 of the licence says.
