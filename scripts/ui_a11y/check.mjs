// The accessibility check for the kernel's basic page (UI-01).
//
// It serves the real files in `ui/` under the real content security policy (read from the Rust
// source, so the two cannot drift), stands in for the API with a few fixed answers, drives the page
// in Chrome and runs axe-core in every state a person can reach: signed out, a failed sign-in, a
// locked-out sign-in, signed in, and signed out again, in light and dark. Any violation fails it.
// It also walks the page with the keyboard alone and checks that focus is visible.
//
// It prints counts, and the rule names of any violation. Nothing it uses is a real credential.

import { existsSync, readFileSync } from "node:fs";
import http from "node:http";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import puppeteer from "puppeteer-core";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..", "..");
const axeSource = readFileSync(createRequire(import.meta.url).resolve("axe-core/axe.min.js"), "utf8");

const policy = readFileSync(join(root, "crates/api/src/ui.rs"), "utf8").match(
  /CONTENT_SECURITY_POLICY: &str = "([^"]+)"/,
)?.[1];
if (!policy) throw new Error("the content security policy is not in crates/api/src/ui.rs");

const files = {
  "/ui": ["index.html", "text/html; charset=utf-8"],
  "/ui/": ["index.html", "text/html; charset=utf-8"],
  "/ui/app.js": ["app.js", "text/javascript; charset=utf-8"],
  "/ui/style.css": ["style.css", "text/css; charset=utf-8"],
};

const USER = "user@example.test";
const RIGHT = "the right password";
const TOKEN = "not-a-real-token";
let ready = true;

function readBody(req) {
  return new Promise((done) => {
    let text = "";
    req.on("data", (c) => (text += c));
    req.on("end", () => done(text));
  });
}

const server = http.createServer(async (req, res) => {
  const path = req.url.split("?")[0];
  const page = files[path];
  if (req.method === "GET" && page) {
    res.writeHead(200, {
      "content-type": page[1],
      "content-security-policy": policy,
      "cache-control": "no-store",
    });
    res.end(readFileSync(join(root, "ui", page[0])));
  } else if (req.method === "GET" && path === "/__ready/off") {
    ready = false;
    res.writeHead(204);
    res.end();
  } else if (req.method === "GET" && path === "/__ready/on") {
    ready = true;
    res.writeHead(204);
    res.end();
  } else if (req.method === "GET" && path === "/healthz") {
    res.writeHead(ready ? 200 : 503, { "content-type": "application/json" });
    res.end(
      JSON.stringify({
        cell: "cell-test",
        status: ready ? "ok" : "draining",
        accounts_homed: 2,
        leases_held: 2,
        draining: !ready,
      }),
    );
  } else if (path.startsWith("/v1/accounts") && req.headers.authorization !== `Bearer ${TOKEN}`) {
    res.writeHead(401, { "content-type": "application/json" });
    res.end(JSON.stringify({ code: "unauthenticated" }));
  } else if (req.method === "POST" && path === "/v1/accounts") {
    const body = JSON.parse((await readBody(req)) || "{}");
    if (!req.headers["idempotency-key"]) {
      res.writeHead(400, { "content-type": "application/json" });
      res.end(JSON.stringify({ title: "no key" }));
    } else if (body.currency === "XXX") {
      // The server's own words can carry an identifier. The page must not show them.
      res.writeHead(400, { "content-type": "application/json" });
      res.end(JSON.stringify({ title: "acc_secretword is not allowed", detail: "acc_secretword" }));
    } else {
      res.writeHead(201, { "content-type": "application/json" });
      res.end(JSON.stringify({ account_id: "acc_test1", currency: body.currency, status: "open" }));
    }
  } else if (req.method === "GET" && path.startsWith("/v1/accounts/")) {
    const [, , , id, rest] = path.split("/");
    if (id === "acc_missing") {
      res.writeHead(404, { "content-type": "application/json" });
      res.end(JSON.stringify({ title: "acc_missing was not found" }));
    } else if (id !== "acc_test1") {
      res.writeHead(404);
      res.end();
    } else if (rest === "balances") {
      const m = (minor) => ({ minor, currency: "GBP" });
      res.writeHead(200, { "content-type": "application/json" });
      res.end(
        JSON.stringify({
          account_id: id,
          posted: m(12345),
          held: m(2000),
          available: m(60345),
          arranged_overdraft: m(50000),
        }),
      );
    } else {
      res.writeHead(200, { "content-type": "application/json" });
      res.end(
        JSON.stringify({
          account_id: id,
          currency: "GBP",
          status: "open",
          home: { cell: "cell-test", epoch: 1 },
        }),
      );
    }
  } else if (req.method === "POST" && path === "/v1/sessions") {
    const body = JSON.parse((await readBody(req)) || "{}");
    if (body.username === "locked@example.test") {
      res.writeHead(429, { "content-type": "application/json" });
      res.end(JSON.stringify({ code: "too_many_attempts" }));
    } else if (body.username === USER && body.password === RIGHT) {
      res.writeHead(201, { "content-type": "application/json" });
      res.end(JSON.stringify({ token: TOKEN, expires_in: 1800, principal: "usr_test" }));
    } else {
      res.writeHead(401, { "content-type": "application/json" });
      res.end(JSON.stringify({ code: "invalid_credentials" }));
    }
  } else if (req.method === "POST" && path === "/v1/sessions:end") {
    res.writeHead(204);
    res.end();
  } else {
    res.writeHead(404);
    res.end();
  }
});
await new Promise((ready) => server.listen(0, "127.0.0.1", ready));
const base = `http://127.0.0.1:${server.address().port}`;

// Puppeteer does not search the path, so find the browser here: CHROME_PATH, or the first of the
// usual names in a directory on PATH.
function findBrowser() {
  if (process.env.CHROME_PATH) return process.env.CHROME_PATH;
  const names = ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser"];
  for (const dir of (process.env.PATH || "").split(":")) {
    for (const name of names) {
      if (dir && existsSync(join(dir, name))) return join(dir, name);
    }
  }
  throw new Error("no Chrome found: set CHROME_PATH to its full path");
}

const browser = await puppeteer.launch({
  executablePath: findBrowser(),
  headless: true,
  args: ["--no-sandbox", "--disable-gpu"],
});

const problems = [];
let states = 0;
let violations = 0;
let others = 0;

async function audit(page, name) {
  states += 1;
  await page.evaluate(axeSource);
  const result = await page.evaluate(() => axe.run(document));
  for (const v of result.violations) {
    violations += v.nodes.length;
    problems.push(`${name}: ${v.id} (${v.nodes.length})`);
  }
}

async function type(page, selector, text) {
  await page.focus(selector);
  await page.keyboard.type(text);
}

const axeProblems = () => problems.filter((p) => /\(\d+\)$/.test(p)).length;
try {
  for (const scheme of ["light", "dark"]) {
    const page = await browser.newPage();
    await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: scheme }]);
    page.on("console", (m) => {
      if (/content security policy/i.test(m.text())) problems.push(`${scheme}: ${m.text()}`);
    });
    page.on("pageerror", (e) => problems.push(`${scheme}: page error ${e.message}`));
    await page.goto(`${base}/ui`);
    await page.waitForFunction(() => document.getElementById("cell").textContent.includes("cell-test"));
    await audit(page, `${scheme} signed out`);

    // A failed sign-in, then a locked-out one.
    await type(page, "#username", USER);
    await type(page, "#password", "wrong");
    await page.keyboard.press("Enter");
    await page.waitForFunction(() => !document.getElementById("sign-in-error").hidden);
    await audit(page, `${scheme} failed sign-in`);
    await page.$eval("#username", (e) => (e.value = ""));
    await type(page, "#username", "locked@example.test");
    await type(page, "#password", "anything");
    await page.keyboard.press("Enter");
    await page.waitForFunction(() =>
      document.getElementById("sign-in-error").textContent.includes("Too many"),
    );
    await audit(page, `${scheme} locked out`);

    // The keyboard alone: from the top, to the fields, the button, in and out.
    await page.reload();
    await page.waitForFunction(() => document.getElementById("cell").textContent.includes("cell-test"));
    const order = [];
    for (let i = 0; i < 3; i += 1) {
      await page.keyboard.press("Tab");
      order.push(await page.evaluate(() => document.activeElement.id));
      const outline = await page.evaluate(() => {
        const s = getComputedStyle(document.activeElement);
        return s.outlineStyle !== "none" && parseFloat(s.outlineWidth) >= 2;
      });
      if (!outline) problems.push(`${scheme}: no visible focus on ${order.at(-1)}`);
    }
    if (order.join() !== "username,password,sign-in-button") {
      problems.push(`${scheme}: tab order was ${order.join()}`);
    }
    await page.focus("#username");
    await page.keyboard.type(USER);
    await page.keyboard.press("Tab");
    await page.keyboard.type(RIGHT);
    await page.keyboard.press("Enter");
    await page.waitForFunction(() => !document.getElementById("signed-in").hidden);
    if ((await page.$eval("#who", (e) => e.textContent)) !== "usr_test") {
      problems.push(`${scheme}: the principal was not shown`);
    }
    if ((await page.evaluate(() => document.activeElement.id)) !== "signed-in-heading") {
      problems.push(`${scheme}: focus did not move to the signed-in heading`);
    }
    await page.waitForFunction(() => document.getElementById("ready").textContent.includes("ready"));
    await audit(page, `${scheme} signed in`);

    // The keyboard alone, through the whole signed-in page.
    const stops = [];
    for (let i = 0; i < 7; i += 1) {
      await page.keyboard.press("Tab");
      stops.push(await page.evaluate(() => document.activeElement.id));
      const outline = await page.evaluate(() => {
        const s = getComputedStyle(document.activeElement);
        return s.outlineStyle !== "none" && parseFloat(s.outlineWidth) >= 2;
      });
      if (!outline) problems.push(`${scheme}: no visible focus on ${stops.at(-1)}`);
    }
    if (stops.join() !== "sign-out,check,currency,overdraft,open-button,account,read-button") {
      problems.push(`${scheme}: signed-in tab order was ${stops.join()}`);
    }

    const said = (id) => page.$eval(`#${id}`, (e) => e.textContent);
    const visible = (id) => page.$eval(`#${id}`, (e) => !e.hidden);
    const waitFor = (id) => page.waitForFunction((i) => !document.getElementById(i).hidden, {}, id);

    // Open an account: first a mistake caught at once, then one the server refuses, then it works.
    await page.$eval("#currency", (e) => (e.value = ""));
    await type(page, "#currency", "GB");
    await page.click("#open-button");
    await waitFor("open-error");
    await audit(page, `${scheme} open: currency mistake`);
    await page.$eval("#currency", (e) => (e.value = ""));
    await type(page, "#currency", "XXX");
    await page.click("#open-button");
    await page.waitForFunction(() => document.getElementById("open-error").textContent.includes("did not accept"));
    if ((await said("open-error")).includes("acc_secretword")) {
      problems.push(`${scheme}: the server's own words were shown`);
    }
    await audit(page, `${scheme} open: refused by the cell`);
    await page.$eval("#currency", (e) => (e.value = ""));
    await type(page, "#currency", "gbp");
    await page.$eval("#overdraft", (e) => (e.value = ""));
    await type(page, "#overdraft", "50000");
    await page.keyboard.press("Enter");
    await waitFor("opened");
    if ((await page.$eval("#account", (e) => e.value)) !== "acc_test1") {
      problems.push(`${scheme}: the new account number was not put in the box`);
    }
    if (await visible("open-error")) problems.push(`${scheme}: an old error was left showing`);
    await audit(page, `${scheme} open: account opened`);

    // Read it: the amounts are whole minor units and a currency, never a decimal.
    await page.focus("#account");
    await page.keyboard.press("Enter");
    await waitFor("account-view");
    const amounts = [];
    for (const id of ["b-posted", "b-held", "b-available", "b-overdraft"]) amounts.push(await said(id));
    if (amounts.join() !== "12345 GBP,2000 GBP,60345 GBP,50000 GBP") {
      problems.push(`${scheme}: amounts were shown as ${amounts.join()}`);
    }
    if (amounts.some((a) => /[.,£$]/.test(a))) problems.push(`${scheme}: an amount has a decimal or a symbol`);
    if ((await page.evaluate(() => document.activeElement.id)) !== "account-heading") {
      problems.push(`${scheme}: focus did not move to the account`);
    }
    await audit(page, `${scheme} read: account shown`);
    await page.$eval("#account", (e) => (e.value = ""));
    await type(page, "#account", "acc_missing");
    await page.keyboard.press("Enter");
    await waitFor("read-error");
    if (await visible("account-view")) problems.push(`${scheme}: an old account was left showing`);
    if ((await said("read-error")).includes("acc_missing")) {
      problems.push(`${scheme}: an identifier was repeated in an error`);
    }
    await audit(page, `${scheme} read: not found`);
    await page.$eval("#account", (e) => (e.value = ""));
    await type(page, "#account", "nope");
    await page.keyboard.press("Enter");
    await page.waitForFunction(() => document.getElementById("read-error").textContent.includes("starts with"));
    await audit(page, `${scheme} read: not an account number`);

    // A cell that is not ready says so.
    await fetch(`${base}/__ready/off`);
    await page.click("#check");
    await page.waitForFunction(() => document.getElementById("ready").textContent.includes("draining"));
    await audit(page, `${scheme} cell draining`);
    await fetch(`${base}/__ready/on`);

    await page.focus("#sign-out");
    await page.keyboard.press("Enter");
    await page.waitForFunction(() => !document.getElementById("signed-out").hidden);
    if ((await page.evaluate(() => document.activeElement.id)) !== "username") {
      problems.push(`${scheme}: focus did not return to the username`);
    }
    // Nothing the person typed or was given is left in the page or in storage.
    const left = await page.evaluate(() => ({
      stored: localStorage.length + sessionStorage.length,
      cookies: document.cookie.length,
      password: document.getElementById("password").value,
      who: document.getElementById("who").textContent,
      account: document.getElementById("account").value,
      posted: document.getElementById("b-posted").textContent,
      opened: document.getElementById("opened").textContent,
      url: location.href,
    }));
    if (
      left.stored || left.cookies || left.password || left.who || left.account || left.posted ||
      left.opened || left.url.includes("?")
    ) {
      problems.push(`${scheme}: something was left behind after signing out`);
    }
    await audit(page, `${scheme} signed out again`);
    await page.close();
  }
} finally {
  await browser.close();
  server.close();
}

others = problems.length - axeProblems();
console.log(`states checked: ${states}, violations: ${violations}, other problems: ${others}`);
if (problems.length > 0) {
  for (const p of problems) console.log(`  ${p}`);
  process.exit(1);
}
