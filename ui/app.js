// The basic page. Plain JavaScript, no framework and no request to any other site.
//
// The token a sign-in returns is kept in `session` below and nowhere else: not in storage, not in
// a cookie, not in a URL and not in any log. Reloading the page signs you out.
//
// Two rules for what the page says. Money is shown as whole minor units and its currency, and is
// never turned into a decimal number, so there is no rounding to get wrong. And a failure is said
// in the page's own words: nothing the server sends in an error is shown, because it could carry an
// identifier.

const $ = (id) => document.getElementById(id);

const session = { token: null, timer: null };
// The key of an account opening that has been sent and not yet answered, so that sending the same
// form again after a lost reply opens one account, not two.
let pending = null;
// The key of the transfer on the page. It is made when the details are first sent and kept while
// they stay the same, even after the cell answers, so pressing the button again is a repeat and
// never a second transfer. Changing any detail, or starting a new transfer, makes a new key.
let moving = null;

const SIGN_IN_MESSAGES = {
  401: "That username and password were not accepted.",
  404: "Sign-in is not set up on this cell.",
  429: "Too many tries. Wait a minute, then try again.",
};

function say(status) {
  if (status === 400) return "The cell did not accept that. Check what you typed.";
  if (status === 404) return "There is no such account on this cell.";
  if (status === 409) return "That was used before with different details. Change something and try again.";
  if (status === 421) return "That account is kept at a different cell. Use that cell's page.";
  if (status === 422) {
    return "The cell declined that. The paying account may not have enough available, or the currency may not match the accounts.";
  }
  if (status === 429) return "Too many requests. Wait a moment, then try again.";
  if (status === 503) return "This cell is not ready, or is draining. Check the cell, then try again.";
  if (status >= 500) return "Something went wrong at the cell. Try again.";
  return "That did not work. Try again.";
}

function showError(id, text) {
  const box = $(id);
  box.textContent = text;
  box.hidden = text === "";
}

function show(signedIn) {
  $("signed-out").hidden = signedIn;
  $("signed-in").hidden = !signedIn;
}

// ---- The cell ----

function readiness(body) {
  const counts = `It holds the leases for ${body.leases_held} of ${body.accounts_homed} accounts kept here.`;
  if (body.status === "ok") return `This cell is ready. ${counts}`;
  if (body.status === "draining") return "This cell is draining. It takes no new work.";
  if (body.status === "leases_missing") return `This cell is not ready. ${counts}`;
  if (body.status === "control_plane_unavailable") {
    return "This cell cannot reach its control plane, so it is not ready.";
  }
  return "This cell is not ready.";
}

async function checkCell() {
  const line = $("cell");
  const ready = $("ready");
  try {
    const reply = await fetch("/healthz", { cache: "no-store", credentials: "omit" });
    const body = await reply.json();
    line.textContent = `Served by cell ${body.cell}.`;
    ready.textContent = readiness(body);
  } catch {
    line.textContent = "This cell did not answer.";
    ready.textContent = "This cell did not answer.";
  }
}

// ---- The session ----

function forget() {
  clearTimeout(session.timer);
  session.token = null;
  session.timer = null;
  pending = null;
  moving = null;
  $("who").textContent = "";
  $("ready").textContent = "Not checked yet.";
  $("currency").value = "GBP";
  $("overdraft").value = "0";
  $("account").value = "";
  for (const id of ["open-error", "read-error", "move-error"]) showError(id, "");
  clearTransfer();
  $("payer").value = "";
  $("payee").value = "";
  $("opened").hidden = true;
  $("opened").textContent = "";
  $("account-view").hidden = true;
  for (const id of ["v-currency", "v-status", "v-cell", "b-posted", "b-held", "b-available", "b-overdraft"]) {
    $(id).textContent = "";
  }
}

function signedOut(message) {
  forget();
  show(false);
  showError("sign-in-error", message);
  $("username").focus();
}

async function signIn(event) {
  event.preventDefault();
  showError("sign-in-error", "");
  const button = $("sign-in-button");
  button.disabled = true;
  try {
    const reply = await fetch("/v1/sessions", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        username: $("username").value.trim(),
        password: $("password").value,
      }),
      cache: "no-store",
      credentials: "omit",
    });
    if (!reply.ok) {
      $("password").value = "";
      showError("sign-in-error", SIGN_IN_MESSAGES[reply.status] ?? "Sign-in did not work. Try again.");
      $("password").focus();
      return;
    }
    const issued = await reply.json();
    session.token = issued.token;
    session.timer = setTimeout(
      () => signedOut("Your session ended. Sign in again."),
      issued.expires_in * 1000,
    );
    $("who").textContent = issued.principal;
    $("username").value = "";
    $("password").value = "";
    show(true);
    $("signed-in-heading").focus();
    checkCell();
  } catch {
    showError("sign-in-error", "This cell did not answer. Try again.");
  } finally {
    button.disabled = false;
  }
}

async function signOut() {
  const token = session.token;
  forget();
  if (token !== null) {
    try {
      await fetch("/v1/sessions:end", {
        method: "POST",
        headers: { authorization: `Bearer ${token}` },
        cache: "no-store",
        credentials: "omit",
      });
    } catch {
      // The token still stops at its own expiry.
    }
  }
  signedOut("");
}

// ---- Calls with the token ----

// Returns { status, body } and never throws. A reply that says the token is no good ends the
// session, whatever was being asked.
async function call(path, options = {}) {
  try {
    const headers = { authorization: `Bearer ${session.token}` };
    if (options.body !== undefined) headers["content-type"] = "application/json";
    if (options.key !== undefined) headers["idempotency-key"] = options.key;
    const reply = await fetch(path, {
      method: options.method ?? "GET",
      headers,
      body: options.body === undefined ? undefined : JSON.stringify(options.body),
      cache: "no-store",
      credentials: "omit",
    });
    if (reply.status === 401) {
      signedOut("Your session ended. Sign in again.");
      return { status: 401, body: null };
    }
    let body = null;
    try {
      body = await reply.json();
    } catch {
      // A reply with no body, or one that is not JSON.
    }
    return { status: reply.status, body };
  } catch {
    return { status: 0, body: null };
  }
}

function fail(errorId, result) {
  if (result.status === 401) return;
  showError(errorId, result.status === 0 ? "This cell did not answer. Try again." : say(result.status));
}

// ---- Money ----

// Whole minor units and the currency, as the server sent them. A number too big to be exact is
// refused rather than shown wrong.
function money(value) {
  if (
    value === null ||
    typeof value !== "object" ||
    !Number.isSafeInteger(value.minor) ||
    typeof value.currency !== "string"
  ) {
    return null;
  }
  return `${value.minor} ${value.currency}`;
}

// ---- Open an account ----

async function openAccount(event) {
  event.preventDefault();
  showError("open-error", "");
  $("opened").hidden = true;
  const currency = $("currency").value.trim().toUpperCase();
  const overdraft = $("overdraft").value.trim();
  $("currency").removeAttribute("aria-invalid");
  $("overdraft").removeAttribute("aria-invalid");
  if (!/^[A-Z]{3}$/.test(currency)) {
    $("currency").setAttribute("aria-invalid", "true");
    showError("open-error", "A currency is three letters, such as GBP.");
    $("currency").focus();
    return;
  }
  if (!/^[0-9]{1,15}$/.test(overdraft)) {
    $("overdraft").setAttribute("aria-invalid", "true");
    showError("open-error", "An overdraft limit is a whole number of minor units, such as 0 or 50000.");
    $("overdraft").focus();
    return;
  }
  const form = `${currency} ${overdraft}`;
  if (pending === null || pending.form !== form) {
    pending = { form, key: crypto.randomUUID() };
  }
  const button = $("open-button");
  button.disabled = true;
  const result = await call("/v1/accounts", {
    method: "POST",
    key: pending.key,
    body: { currency, overdraft_limit: { minor: Number(overdraft), currency } },
  });
  button.disabled = false;
  if (result.status !== 201 && result.status !== 200) {
    fail("open-error", result);
    return;
  }
  pending = null;
  const id = result.body?.account_id;
  if (typeof id !== "string") {
    showError("open-error", "The cell opened an account but did not say which. Try again.");
    return;
  }
  $("account").value = id;
  $("opened").textContent = `Account opened. Its number is ${id}, and it is in the box below.`;
  $("opened").hidden = false;
}

// ---- Read an account ----

async function readAccount(event) {
  event.preventDefault();
  showError("read-error", "");
  $("account-view").hidden = true;
  $("account").removeAttribute("aria-invalid");
  const id = $("account").value.trim();
  if (!/^acc_[A-Za-z0-9]{1,64}$/.test(id)) {
    $("account").setAttribute("aria-invalid", "true");
    showError("read-error", "An account number starts with acc_ and has no spaces.");
    $("account").focus();
    return;
  }
  const button = $("read-button");
  button.disabled = true;
  const path = `/v1/accounts/${encodeURIComponent(id)}`;
  const [account, balances] = await Promise.all([call(path), call(`${path}/balances`)]);
  button.disabled = false;
  for (const result of [account, balances]) {
    if (result.status !== 200) {
      fail("read-error", result);
      return;
    }
  }
  const shown = [
    money(balances.body?.posted),
    money(balances.body?.held),
    money(balances.body?.available),
    money(balances.body?.arranged_overdraft),
  ];
  if (shown.includes(null) || typeof account.body?.currency !== "string") {
    showError("read-error", "The cell's answer could not be shown exactly. Try again.");
    return;
  }
  $("v-currency").textContent = account.body.currency;
  $("v-status").textContent = account.body.status;
  $("v-cell").textContent = account.body.home?.cell ?? "";
  $("b-posted").textContent = shown[0];
  $("b-held").textContent = shown[1];
  $("b-available").textContent = shown[2];
  $("b-overdraft").textContent = shown[3];
  $("account-view").hidden = false;
  $("account-heading").focus();
}

// ---- Move money ----

function clearTransfer() {
  moving = null;
  $("amount").value = "";
  $("move-currency").value = "GBP";
  $("key-line").hidden = true;
  $("key").textContent = "";
  $("move-new").hidden = true;
  $("moved").hidden = true;
  $("moved-repeat").textContent = "";
  for (const id of ["m-status", "m-journal", "m-amount", "m-payer", "m-payee", "m-key"]) $(id).textContent = "";
  $("m-watermarks").replaceChildren();
  for (const id of ["payer", "payee", "amount", "move-currency"]) $(id).removeAttribute("aria-invalid");
}

function watermarkRows(list) {
  const rows = [];
  for (const item of list) {
    if (typeof item?.cell !== "string" || !Number.isSafeInteger(item?.watermark)) return null;
    const row = document.createElement("tr");
    const cell = document.createElement("th");
    cell.scope = "row";
    cell.textContent = item.cell;
    const mark = document.createElement("td");
    mark.textContent = String(item.watermark);
    row.append(cell, mark);
    rows.push(row);
  }
  return rows;
}

async function moveMoney(event) {
  event.preventDefault();
  showError("move-error", "");
  for (const id of ["payer", "payee", "amount", "move-currency"]) $(id).removeAttribute("aria-invalid");
  const payer = $("payer").value.trim();
  const payee = $("payee").value.trim();
  const amount = $("amount").value.trim();
  const currency = $("move-currency").value.trim().toUpperCase();
  const mistake = (id, text) => {
    $(id).setAttribute("aria-invalid", "true");
    showError("move-error", text);
    $(id).focus();
  };
  if (!/^acc_[A-Za-z0-9]{1,64}$/.test(payer)) return mistake("payer", "The account to pay from starts with acc_ and has no spaces.");
  if (!/^acc_[A-Za-z0-9]{1,64}$/.test(payee)) return mistake("payee", "The account to pay to starts with acc_ and has no spaces.");
  if (payer === payee) return mistake("payee", "Choose two different accounts.");
  if (!/^[1-9][0-9]{0,14}$/.test(amount)) {
    return mistake("amount", "An amount is a whole number of minor units above 0, such as 2500.");
  }
  if (!/^[A-Z]{3}$/.test(currency)) return mistake("move-currency", "A currency is three letters, such as GBP.");

  const form = `${payer} ${payee} ${amount} ${currency}`;
  if (moving === null || moving.form !== form) {
    moving = { form, key: crypto.randomUUID() };
  }
  $("key").textContent = moving.key;
  $("key-line").hidden = false;
  const button = $("move-button");
  button.disabled = true;
  const result = await call("/v1/transfers", {
    method: "POST",
    key: moving.key,
    body: {
      payer_account_id: payer,
      payee_account_id: payee,
      amount: { minor: Number(amount), currency },
    },
  });
  button.disabled = false;
  if (result.status !== 201 && result.status !== 200) {
    $("moved").hidden = true;
    fail("move-error", result);
    return;
  }
  const body = result.body;
  const shown = money(body?.amount);
  const rows = Array.isArray(body?.watermarks) ? watermarkRows(body.watermarks) : null;
  if (
    shown === null ||
    rows === null ||
    typeof body?.journal_id !== "string" ||
    typeof body?.status !== "string" ||
    typeof body?.replayed !== "boolean" ||
    typeof body?.payer_account_id !== "string" ||
    typeof body?.payee_account_id !== "string"
  ) {
    showError("move-error", "The cell's answer could not be shown exactly. Check the accounts before you try again.");
    return;
  }
  $("moved-repeat").textContent = body.replayed
    ? "This was a repeat of an earlier request with the same key. The cell wrote nothing new."
    : "The transfer was posted. Pressing the button again sends the same key, so it is a repeat and writes nothing new.";
  $("m-status").textContent = body.status;
  $("m-journal").textContent = body.journal_id;
  $("m-amount").textContent = shown;
  $("m-payer").textContent = body.payer_account_id;
  $("m-payee").textContent = body.payee_account_id;
  $("m-key").textContent = moving.key;
  $("m-watermarks").replaceChildren(...rows);
  $("moved").hidden = false;
  $("move-new").hidden = false;
  $("moved-heading").focus();
}

function newTransfer() {
  clearTransfer();
  showError("move-error", "");
  $("payer").focus();
}

$("sign-in").addEventListener("submit", signIn);
$("sign-out").addEventListener("click", signOut);
$("check").addEventListener("click", checkCell);
$("open").addEventListener("submit", openAccount);
$("read").addEventListener("submit", readAccount);
$("move").addEventListener("submit", moveMoney);
$("move-new").addEventListener("click", newTransfer);
checkCell();
