// The basic page against a real API and a real Postgres (UI-03). It is started by the Rust test
// `crates/api/tests/ui_browser.rs`, which serves the API and gives this script its address and the
// test user's sign-in in the environment. It drives the page in Chrome the way a person would: it
// signs in, opens two accounts, moves money, presses the button again, and reads the balances.
//
// It prints counts and the names of what failed, never a token, a password or an account number.

import { existsSync } from "node:fs";
import { join } from "node:path";
import puppeteer from "puppeteer-core";

const base = process.env.E2E_URL;
const username = process.env.E2E_USERNAME;
const password = process.env.E2E_PASSWORD;
if (!base || !username || !password) throw new Error("E2E_URL, E2E_USERNAME and E2E_PASSWORD are needed");

function findBrowser() {
  if (process.env.CHROME_PATH) return process.env.CHROME_PATH;
  for (const dir of (process.env.PATH || "").split(":")) {
    for (const name of ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser"]) {
      if (dir && existsSync(join(dir, name))) return join(dir, name);
    }
  }
  throw new Error("no Chrome found: set CHROME_PATH to its full path");
}

const problems = [];
let checks = 0;
function expect(ok, what) {
  checks += 1;
  if (!ok) problems.push(what);
}

const browser = await puppeteer.launch({
  executablePath: findBrowser(),
  headless: true,
  args: ["--no-sandbox", "--disable-gpu"],
});
try {
  const page = await browser.newPage();
  page.on("pageerror", (e) => problems.push(`page error: ${e.message}`));
  page.on("console", (m) => {
    if (/content security policy/i.test(m.text())) problems.push("the page broke its own content security policy");
  });
  const text = (id) => page.$eval(`#${id}`, (e) => e.textContent);
  const shown = (id) => page.waitForFunction((i) => !document.getElementById(i).hidden, { timeout: 15000 }, id);
  const set = async (id, value) => {
    await page.$eval(`#${id}`, (e) => (e.value = ""));
    await page.focus(`#${id}`);
    await page.keyboard.type(value);
  };

  await page.goto(`${base}/ui`);
  await set("username", username);
  await set("password", password);
  await page.keyboard.press("Enter");
  await shown("signed-in");

  // Two accounts. The payer may go below zero, so no funding is needed.
  await set("currency", "GBP");
  await set("overdraft", "100000");
  await page.click("#open-button");
  await shown("opened");
  const payer = await page.$eval("#account", (e) => e.value);
  await set("overdraft", "0");
  await page.click("#open-button");
  await page.waitForFunction((was) => document.getElementById("account").value !== was, { timeout: 15000 }, payer);
  const payee = await page.$eval("#account", (e) => e.value);
  expect(/^acc_/.test(payer) && /^acc_/.test(payee) && payer !== payee, "two different accounts were opened");

  // A transfer, then the same details again.
  await set("payer", payer);
  await set("payee", payee);
  await set("amount", "2500");
  await page.click("#move-button");
  await shown("moved");
  const key = await text("key");
  const journal = await text("m-journal");
  expect(/^[0-9a-f-]{36}$/.test(key) && (await text("m-key")) === key, "the page made and showed a request key");
  expect(/^jr_/.test(journal), "a journal was named");
  expect((await text("m-status")) === "posted", "the transfer was posted");
  expect((await text("m-amount")) === "2500 GBP", "the amount was shown in minor units");
  expect((await text("moved-repeat")).includes("was posted"), "the first press was not a repeat");

  await page.click("#move-button");
  await page.waitForFunction(() => document.getElementById("moved-repeat").textContent.includes("repeat of"), {
    timeout: 15000,
  });
  expect((await text("key")) === key, "the second press sent the same key");
  expect((await text("m-journal")) === journal, "the repeat named the same journal");

  // The money moved once, not twice.
  const balance = async (account) => {
    await set("account", account);
    await page.click("#read-button");
    await page.waitForFunction(
      (a) => !document.getElementById("account-view").hidden && document.getElementById("account").value === a,
      { timeout: 15000 },
      account,
    );
    return text("b-posted");
  };
  expect((await balance(payee)) === "2500 GBP", "the payee holds the amount once");
  expect((await balance(payer)) === "-2500 GBP", "the payer paid it once");

  // A different amount is a new request with its own key and journal.
  await set("payer", payer);
  await set("payee", payee);
  await set("amount", "100");
  await page.click("#move-button");
  await page.waitForFunction(() => document.getElementById("m-amount").textContent === "100 GBP", { timeout: 15000 });
  expect((await text("key")) !== key, "a changed amount made a new key");
  expect((await text("m-journal")) !== journal, "a changed amount made a new journal");
  expect((await balance(payee)) === "2600 GBP", "the second transfer was added");

  // A transfer the cell declines is said in the page's words.
  await set("payer", payee);
  await set("payee", payer);
  await set("amount", "999999");
  await page.click("#move-button");
  await page.waitForFunction(() => !document.getElementById("move-error").hidden, { timeout: 15000 });
  expect((await text("move-error")).includes("declined"), "a transfer without enough was declined");
  expect(!(await text("move-error")).includes(payer), "no account number was repeated in the error");
} finally {
  await browser.close();
}
console.log(`browser checks: ${checks}, problems: ${problems.length}`);
for (const p of problems) console.log(`problem: ${p}`);
process.exit(problems.length === 0 ? 0 : 1);
