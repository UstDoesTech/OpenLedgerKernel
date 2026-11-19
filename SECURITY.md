# Security

## Reporting a vulnerability

Please report it privately, not in a public issue, pull request or discussion.

Use GitHub's private reporting: **Security, Report a vulnerability** on this repository. Say what you found, how to reproduce it, and what it lets someone do. You will get a first reply within seven days.

If something you found lets money move without the rules being applied, or lets one tenant reach another's data, say so at the top. Those are the guarantees this project exists to keep: append-only entries, epoch fencing, idempotency, tenant isolation, and no personal data in the ledger.

## What to leave out of anything public

This repository is public, and so are its workflow logs and issues. Never post:

- account, subscription, project or tenant identifiers, bucket or vault names, or the output of a plan (a plan lists them);
- keys, tokens, pre-shared keys or `TF_VAR_` values, even old ones;
- personal data.

If you posted one by mistake, tell the maintainer privately, and treat it as exposed. Deleting a post does not undo it. [`docs/runbooks/troubleshooting.md`](docs/runbooks/troubleshooting.md) says what to do next.

## Supported versions

The project has no released versions yet. Security fixes go to `main`.

## How the repository protects itself

- Workflows that hold cloud access run only on manual dispatch, only from `main`, only for the owner, and wait for an approved environment. See [`docs/credentialed-workflows.md`](docs/credentialed-workflows.md).
- No cloud secret is stored in GitHub. Each cloud trusts one environment of this repository.
- Their logs hold counts and a fingerprint, never plan text. Values that identify an estate are secrets, so GitHub masks them.
- Third-party actions are pinned to a full commit, and Dependabot proposes updates.
