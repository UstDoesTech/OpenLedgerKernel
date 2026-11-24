// The accessibility check for the kernel's basic page (UI-01).
//
// It serves the real files in `ui/` under the real content security policy (read from the Rust
// source, so the two cannot drift), stands in for the API with a few fixed answers, drives the page
// in Chrome and runs axe-core in every state a person can reach: signed out, a failed sign-in, a
// locked-out sign-in, signed in, and signed out again, in light and dark. Any violation fails it.
// It also walks the page with the keyboard alone and checks that focus is visible.
//
// It prints counts, and the rule names of any violation. Nothing it uses is a real credential.

import { readFileSync } from "node:fs";
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
  } else if (req.method === "GET" && path === "/healthz") {
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify({ cell: "cell-test", status: "ok" }));
  } else if (req.method === "POST" && path === "/v1/sessions") {
    const body = JSON.parse((await readBody(req)) || "{}");
    if (body.username === "locked@example.test") {
      res.writeHead(429, { "content-type": "application/json" });
      res.end(JSON.stringify({ code: "too_many_attempts" }));
    } else if (body.username === USER && body.password === RIGHT) {
      res.writeHead(201, { "content-type": "application/json" });
      res.end(JSON.stringify({ token: "not-a-real-token", expires_in: 1800, principal: "usr_test" }));
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

const browser = await puppeteer.launch({
  executablePath: process.env.CHROME_PATH || "google-chrome",
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
    await audit(page, `${scheme} signed in`);
    await page.keyboard.press("Tab");
    if ((await page.evaluate(() => document.activeElement.id)) !== "sign-out") {
      problems.push(`${scheme}: sign out is not the next stop`);
    }
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
      url: location.href,
    }));
    if (left.stored || left.cookies || left.password || left.who || left.url.includes("?")) {
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
