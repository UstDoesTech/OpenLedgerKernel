# The basic page

A page the API serves at `/ui`, so a person can try a cell: see which cell it is and whether it is ready, sign in with a username and password, open an account in a currency with an overdraft limit, and read an account and its posted, held and available balances. Making transfers comes with UI-03.

It is plain HTML, CSS and JavaScript. There is no framework, no build step and no request to any other site (D-20). The three files are compiled into the API's binary by `crates/api/src/ui.rs`, so the page and the API share an origin.

- **The token** a sign-in returns is held in a variable and nowhere else: not in storage, a cookie, a URL or a log. Reloading the page signs you out.
- **Money** is shown as whole minor units and the currency, such as `12345 GBP`. The page never turns an amount into a decimal, and refuses one too large to show exactly.
- **Failures** are said in the page's own words, chosen from the status alone. Nothing the server sends in an error is shown, because it could quote an identifier.
- **Every response** carries a content security policy that allows only the page's own files, refuses framing, and says nothing is to be cached.
- **Accessibility** is checked in CI by `scripts/ui_a11y/check.mjs`, which runs axe-core in Chrome in every state (signed out, a failed sign-in, a locked-out one, signed in, signed out again; light and dark) and walks the page with the keyboard alone. To run it yourself: `cd scripts/ui_a11y && npm ci && node check.mjs`.

This is a way to try the kernel. It is not the commercial console.
