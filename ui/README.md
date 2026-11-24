# The basic page

A page the API serves at `/ui`, so a person can try a cell: see which cell it is and whether it is ready, sign in with a username and password, open an account in a currency with an overdraft limit, read an account and its posted, held and available balances, and move money between two accounts, at this cell or at another.

It is plain HTML, CSS and JavaScript. There is no framework, no build step and no request to any other site (D-20). The three files are compiled into the API's binary by `crates/api/src/ui.rs`, so the page and the API share an origin.

- **The token** a sign-in returns is held in a variable and nowhere else: not in storage, a cookie, a URL or a log. Reloading the page signs you out.
- **Money** is shown as whole minor units and the currency, such as `12345 GBP`. The page never turns an amount into a decimal, and refuses one too large to show exactly.
- **A transfer's key** is made by the page and shown. It is kept while the details stay the same, so pressing the button again is a repeat the cell recognises and writes nothing for. Changing a detail, or choosing *Start a new transfer*, makes a new key. The page shows whether the answer was a repeat, the journal and where it was recorded.
- **Failures** are said in the page's own words, chosen from the status alone. Nothing the server sends in an error is shown, because it could quote an identifier.
- **Every response** carries a content security policy that allows only the page's own files, refuses framing, and says nothing is to be cached.
- **Accessibility** is checked in CI by `scripts/ui_a11y/check.mjs`, which runs axe-core in Chrome in every state (signed out, a failed sign-in, a locked-out one, signed in, signed out again; light and dark) and walks the page with the keyboard alone. To run it yourself: `cd scripts/ui_a11y && npm ci && node check.mjs`.
- **A real browser against a real API** is `crates/api/tests/ui_browser.rs`, run in the integration job. It serves the API over Postgres, drives the page through `scripts/ui_a11y/e2e.mjs`, and counts the journals in the database to prove a repeat wrote nothing.

This is a way to try the kernel. It is not the commercial console.
