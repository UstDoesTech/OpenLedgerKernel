// The basic page. Plain JavaScript, no framework and no request to any other site.
//
// The token a sign-in returns is kept in `session` below and nowhere else: not in storage, not in
// a cookie, not in a URL and not in any log. Reloading the page signs you out.

const $ = (id) => document.getElementById(id);

const session = { token: null, timer: null };

const MESSAGES = {
  401: "That username and password were not accepted.",
  404: "Sign-in is not set up on this cell.",
  429: "Too many tries. Wait a minute, then try again.",
};

function showError(text) {
  const box = $("sign-in-error");
  box.textContent = text;
  box.hidden = text === "";
}

function show(signedIn) {
  $("signed-out").hidden = signedIn;
  $("signed-in").hidden = !signedIn;
}

async function showCell() {
  const line = $("cell");
  try {
    const reply = await fetch("/healthz", { cache: "no-store", credentials: "omit" });
    const body = await reply.json();
    line.textContent = `Served by cell ${body.cell}. Its status is ${body.status}.`;
  } catch {
    line.textContent = "This cell did not answer.";
  }
}

function forget() {
  clearTimeout(session.timer);
  session.token = null;
  session.timer = null;
  $("who").textContent = "";
}

function signedOut(message) {
  forget();
  show(false);
  showError(message);
  $("username").focus();
}

async function signIn(event) {
  event.preventDefault();
  showError("");
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
      showError(MESSAGES[reply.status] ?? "Sign-in did not work. Try again.");
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
  } catch {
    showError("This cell did not answer. Try again.");
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

$("sign-in").addEventListener("submit", signIn);
$("sign-out").addEventListener("click", signOut);
showCell();
