//! Sign in with a username and password (task K-21, decision D-21).
//!
//! A person's username and password are checked against a list of users, kept as Argon2id hashes
//! and read from a file that is read again when it changes. A correct password gets a random
//! token that lasts 30 minutes. The token is held in this cell's memory, as a hash, and the
//! existing bearer check accepts it. The user's tenant and principal come from the list and never
//! from the request. A username is personal data, so it never reaches a journal, a log or a reply.
//!
//! This is a stand-in for real sign-in (task A-06), which replaces it behind the same
//! [`Authenticator`] trait.
//!
//! What it keeps, always:
//! - an unknown user and a wrong password get the same answer, and an unknown user is checked
//!   against a throwaway hash, so the time does not say which it was;
//! - five failures in a row lock the username for a minute, and a correct password does not lift
//!   the lock early, so a guess cannot be tried again at once;
//! - at most a few hashes run at once, so a flood of sign-ins cannot starve the ledger.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use argon2::{Argon2, PasswordHasher, PasswordVerifier};
use ledger_core::TenantId;
use ring::rand::{SecureRandom, SystemRandom};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

use crate::auth::{Authenticator, Principal, is_principal_name};

/// How long a sign-in lasts.
pub const SESSION_LIFETIME: Duration = Duration::from_secs(30 * 60);
/// Failures in a row before the username is locked.
pub const FAILURES_BEFORE_LOCK: u32 = 5;
/// How long a lock lasts.
pub const LOCK_FOR: Duration = Duration::from_secs(60);
/// Hashes that may run at the same time.
const CONCURRENT_HASHES: usize = 4;
/// Usernames and sessions kept in memory.
const MAX_TRACKED: usize = 10_000;
const MAX_PASSWORD_BYTES: usize = 1024;
const MAX_USERNAME_BYTES: usize = 254;

/// Time since some fixed start, so a test can move it.
pub trait Clock: Send + Sync {
    fn now(&self) -> Duration;
}

/// The real clock: monotonic, so a change to the machine's time cannot extend a session.
#[derive(Debug)]
pub struct MonotonicClock(Instant);

impl MonotonicClock {
    pub fn new() -> Self {
        MonotonicClock(Instant::now())
    }
}

impl Default for MonotonicClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for MonotonicClock {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }
}

/// Why a user list was refused.
#[derive(Debug, thiserror::Error)]
pub enum UsersError {
    #[error("the user list is not valid: {0}")]
    Invalid(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    username: String,
    password_hash: String,
    tenant: String,
    principal: String,
}

#[derive(Debug)]
struct Record {
    hash: String,
    principal: Principal,
}

/// The users who may sign in, by username in lower case.
#[derive(Debug)]
pub struct Users {
    by_name: HashMap<String, Record>,
    /// A hash nobody has the password for, checked when the username is unknown.
    dummy: String,
}

/// The username as it is looked up: lower case, with one `@`, printable and without spaces.
fn normalise(username: &str) -> Option<String> {
    if username.is_empty()
        || username.len() > MAX_USERNAME_BYTES
        || !username.bytes().all(|b| b.is_ascii_graphic())
    {
        return None;
    }
    let (name, domain) = username.split_once('@')?;
    if name.is_empty() || domain.is_empty() || domain.contains('@') {
        return None;
    }
    Some(username.to_ascii_lowercase())
}

impl Users {
    pub fn empty() -> Self {
        Users {
            by_name: HashMap::new(),
            dummy: dummy_hash(),
        }
    }

    pub fn from_json(text: &str) -> Result<Self, UsersError> {
        let entries: Vec<Entry> = serde_json::from_str(text)
            .map_err(|_| UsersError::Invalid("not a JSON list of users".to_owned()))?;
        let mut users = Users::empty();
        for (n, e) in entries.into_iter().enumerate() {
            // Never repeat a username or a hash: the list is read from a secret.
            let bad = |why: &str| UsersError::Invalid(format!("user {}: {why}", n + 1));
            let name =
                normalise(&e.username).ok_or_else(|| bad("the username is not an address"))?;
            if !e.password_hash.starts_with("$argon2id$")
                || argon2::password_hash::phc::PasswordHash::new(&e.password_hash).is_err()
            {
                return Err(bad("the password is not kept as an Argon2id hash"));
            }
            let tenant: TenantId = e
                .tenant
                .parse()
                .map_err(|_| bad("the tenant is not valid"))?;
            if !is_principal_name(&e.principal) {
                return Err(bad("the principal is not a principal name"));
            }
            let record = Record {
                hash: e.password_hash,
                principal: Principal {
                    tenant,
                    name: e.principal,
                },
            };
            if users.by_name.insert(name, record).is_some() {
                return Err(bad("the username is listed twice"));
            }
        }
        Ok(users)
    }

    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }
}

/// A hash of a password nobody chose, with a fixed salt. It is made once, when the list is read.
fn dummy_hash() -> String {
    Argon2::default()
        .hash_password_with_salt(b"nobody chose this password", b"a fixed salt!!!!")
        .map(|h| h.to_string())
        .unwrap_or_default()
}

fn hash_token(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

/// The tokens that sign-ins have made, held as hashes with the time each stops working.
pub struct Sessions {
    clock: Arc<dyn Clock>,
    lifetime: Duration,
    live: Mutex<HashMap<[u8; 32], (Principal, Duration)>>,
}

impl Sessions {
    pub fn new(clock: Arc<dyn Clock>, lifetime: Duration) -> Self {
        Sessions {
            clock,
            lifetime,
            live: Mutex::new(HashMap::new()),
        }
    }

    /// A new token for a principal, or none if too many are live or no randomness is available.
    fn issue(&self, principal: &Principal) -> Option<String> {
        let mut raw = [0u8; 32];
        SystemRandom::new().fill(&mut raw).ok()?;
        let token: String = raw.iter().map(|b| format!("{b:02x}")).collect();
        let now = self.clock.now();
        let mut live = self.live.lock().ok()?;
        if live.len() >= MAX_TRACKED {
            live.retain(|_, (_, until)| *until > now);
            if live.len() >= MAX_TRACKED {
                return None;
            }
        }
        live.insert(hash_token(&token), (principal.clone(), now + self.lifetime));
        Some(token)
    }

    /// Ends a token's sign-in. Ending one that is not live does nothing.
    pub fn end(&self, token: &str) {
        if let Ok(mut live) = self.live.lock() {
            live.remove(&hash_token(token));
        }
    }

    pub fn live(&self) -> usize {
        self.live.lock().map(|l| l.len()).unwrap_or(0)
    }
}

impl Authenticator for Sessions {
    fn authenticate(&self, token: &str) -> Option<Principal> {
        let key = hash_token(token);
        let now = self.clock.now();
        let mut live = self.live.lock().ok()?;
        match live.get(&key) {
            Some((principal, until)) if *until > now => Some(principal.clone()),
            Some(_) => {
                live.remove(&key);
                None
            }
            None => None,
        }
    }
}

/// Failed sign-ins in a row for each username, and until when a username is locked.
#[derive(Default)]
struct Throttle {
    by_name: Mutex<HashMap<String, (u32, Option<Duration>)>>,
}

impl Throttle {
    fn locked(&self, name: &str, now: Duration) -> bool {
        let Ok(map) = self.by_name.lock() else {
            return true;
        };
        matches!(map.get(name), Some((_, Some(until))) if *until > now)
    }

    fn failed(&self, name: &str, now: Duration) {
        let Ok(mut map) = self.by_name.lock() else {
            return;
        };
        if map.len() >= MAX_TRACKED && !map.contains_key(name) {
            // Forget those that are neither locked nor close to it. A lock is never dropped.
            map.retain(|_, (_, until)| until.is_some_and(|u| u > now));
            if map.len() >= MAX_TRACKED {
                return;
            }
        }
        let entry = map.entry(name.to_owned()).or_insert((0, None));
        if matches!(entry.1, Some(until) if until <= now) {
            *entry = (0, None);
        }
        entry.0 += 1;
        if entry.0 >= FAILURES_BEFORE_LOCK {
            *entry = (0, Some(now + LOCK_FOR));
        }
    }

    fn succeeded(&self, name: &str) {
        if let Ok(mut map) = self.by_name.lock() {
            map.remove(name);
        }
    }
}

/// What a sign-in returns.
#[derive(Debug)]
pub struct Issued {
    pub token: String,
    pub expires_in: u64,
    pub principal: String,
}

/// Why a sign-in did not work.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// An unknown user or a wrong password, not told apart.
    Refused,
    /// Too many failures in a row.
    Locked,
    /// No hash could run, or no token could be made.
    Unavailable,
}

/// Sign-in for one cell.
pub struct Login {
    users: crate::reload::Reloadable<Users>,
    sessions: Arc<Sessions>,
    throttle: Throttle,
    gate: Semaphore,
    clock: Arc<dyn Clock>,
}

impl Login {
    pub fn new(users: crate::reload::Reloadable<Users>, clock: Arc<dyn Clock>) -> Arc<Self> {
        Arc::new(Login {
            users,
            sessions: Arc::new(Sessions::new(clock.clone(), SESSION_LIFETIME)),
            throttle: Throttle::default(),
            gate: Semaphore::new(CONCURRENT_HASHES),
            clock,
        })
    }

    pub fn sessions(&self) -> &Arc<Sessions> {
        &self.sessions
    }

    /// Checks a username and password, and makes a token if they are right.
    pub async fn sign_in(&self, username: &str, password: &str) -> Result<Issued, Refusal> {
        if password.is_empty() || password.len() > MAX_PASSWORD_BYTES {
            return Err(Refusal::Refused);
        }
        let name = normalise(username);
        // A username that cannot be one is still counted, as itself, so that nothing says why.
        let key = name
            .clone()
            .unwrap_or_else(|| username.chars().take(64).collect());
        let now = self.clock.now();
        if self.throttle.locked(&key, now) {
            return Err(Refusal::Locked);
        }
        let users = self.users.get();
        let (hash, principal) = match name.as_ref().and_then(|n| users.by_name.get(n)) {
            Some(r) => (r.hash.clone(), Some(r.principal.clone())),
            None => (users.dummy.clone(), None),
        };
        let _permit = self
            .gate
            .acquire()
            .await
            .map_err(|_| Refusal::Unavailable)?;
        let password = password.to_owned();
        let right = tokio::task::spawn_blocking(move || {
            Argon2::default()
                .verify_password(password.as_bytes(), hash.as_str())
                .is_ok()
        })
        .await
        .map_err(|_| Refusal::Unavailable)?;
        match (right, principal) {
            (true, Some(principal)) => {
                let token = self
                    .sessions
                    .issue(&principal)
                    .ok_or(Refusal::Unavailable)?;
                self.throttle.succeeded(&key);
                Ok(Issued {
                    token,
                    expires_in: SESSION_LIFETIME.as_secs(),
                    principal: principal.name,
                })
            }
            _ => {
                self.throttle.failed(&key, self.clock.now());
                Err(Refusal::Refused)
            }
        }
    }

    /// Ends a sign-in.
    pub fn sign_out(&self, token: &str) {
        self.sessions.end(token);
    }
}

impl Authenticator for Login {
    fn authenticate(&self, token: &str) -> Option<Principal> {
        self.sessions.authenticate(token)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::reload::Reloadable;

    const PASSWORD: &str = "correct horse battery staple";
    const NAME: &str = "user@bankofmumanddad.test";

    struct Fake(AtomicU64);

    impl Fake {
        fn move_on(&self, secs: u64) {
            self.0.fetch_add(secs, Ordering::SeqCst);
        }
    }

    impl Clock for Fake {
        fn now(&self) -> Duration {
            Duration::from_secs(self.0.load(Ordering::SeqCst))
        }
    }

    fn hash(password: &str) -> String {
        Argon2::default()
            .hash_password_with_salt(password.as_bytes(), b"a test salt.....")
            .unwrap()
            .to_string()
    }

    fn list(hash: &str) -> String {
        serde_json::json!([{
            "username": NAME,
            "password_hash": hash,
            "tenant": "ten_acme",
            "principal": "usr_ada",
        }])
        .to_string()
    }

    fn login() -> (Arc<Login>, Arc<Fake>, Reloadable<Users>) {
        let clock = Arc::new(Fake(AtomicU64::new(0)));
        let users = Reloadable::new(Users::from_json(&list(&hash(PASSWORD))).unwrap());
        (Login::new(users.clone(), clock.clone()), clock, users)
    }

    #[test]
    fn the_list_loads_and_refuses_what_is_wrong_with_it() {
        let good = hash(PASSWORD);
        assert_eq!(Users::from_json(&list(&good)).unwrap().len(), 1);
        for (why, text) in [
            ("a plain password", list("hunter2")),
            (
                "another kind of hash",
                list("$2b$12$abcdefghijklmnopqrstuuabcdefghijklmnopqrstuvwxyz01234"),
            ),
            ("not a list", "{}".to_owned()),
            (
                "an unknown field",
                list(&good).replace("\"principal\"", "\"extra\":1,\"principal\""),
            ),
            (
                "a username that is not an address",
                list(&good).replace(NAME, "no-at-sign"),
            ),
            (
                "a tenant that is not one",
                list(&good).replace("ten_acme", "acme"),
            ),
            (
                "a principal that is not one",
                list(&good).replace("usr_ada", "Ada"),
            ),
        ] {
            assert!(Users::from_json(&text).is_err(), "{why}");
        }
        let twice = format!(
            "[{0},{0}]",
            list(&good).trim_matches(|c| c == '[' || c == ']')
        );
        assert!(Users::from_json(&twice).is_err(), "a username listed twice");
        // The reason never repeats what was in the list.
        let err = Users::from_json(&list("hunter2")).unwrap_err().to_string();
        assert!(!err.contains("hunter2") && !err.contains(NAME), "{err}");
    }

    #[tokio::test]
    async fn the_right_password_signs_in_and_the_token_names_the_user() {
        let (login, _, _) = login();
        let issued = login.sign_in(NAME, PASSWORD).await.unwrap();
        assert_eq!(issued.expires_in, 1800);
        assert_eq!(issued.principal, "usr_ada");
        assert_eq!(issued.token.len(), 64);
        let who = login.authenticate(&issued.token).unwrap();
        assert_eq!(who.name, "usr_ada");
        assert_eq!(who.tenant, "ten_acme".parse::<TenantId>().unwrap());
        // Another sign-in is another token, and each works.
        let again = login.sign_in(NAME, PASSWORD).await.unwrap();
        assert_ne!(again.token, issued.token);
        assert!(login.authenticate(&issued.token).is_some());
    }

    #[tokio::test]
    async fn the_username_is_not_case_sensitive_and_the_password_is() {
        let (login, _, _) = login();
        assert!(
            login
                .sign_in("User@BankOfMumAndDad.TEST", PASSWORD)
                .await
                .is_ok()
        );
        assert_eq!(
            login
                .sign_in(NAME, &PASSWORD.to_uppercase())
                .await
                .unwrap_err(),
            Refusal::Refused
        );
    }

    #[tokio::test]
    async fn an_unknown_user_and_a_wrong_password_are_not_told_apart() {
        let (login, _, users) = login();
        let wrong = login.sign_in(NAME, "wrong password").await.unwrap_err();
        let unknown = login
            .sign_in("nobody@bankofmumanddad.test", PASSWORD)
            .await
            .unwrap_err();
        let not_an_address = login.sign_in("nobody", PASSWORD).await.unwrap_err();
        assert_eq!(wrong, Refusal::Refused);
        assert_eq!(unknown, Refusal::Refused);
        assert_eq!(not_an_address, Refusal::Refused);
        // An unknown user is checked against a real hash, so there is work to do either way.
        assert!(users.get().dummy.starts_with("$argon2id$"));
    }

    #[tokio::test]
    async fn nothing_is_hashed_for_an_empty_or_huge_password() {
        let (login, _, _) = login();
        assert_eq!(login.sign_in(NAME, "").await.unwrap_err(), Refusal::Refused);
        assert_eq!(
            login
                .sign_in(NAME, &"x".repeat(MAX_PASSWORD_BYTES + 1))
                .await
                .unwrap_err(),
            Refusal::Refused
        );
    }

    #[tokio::test]
    async fn five_failures_lock_the_name_and_the_right_password_does_not_lift_it() {
        let (login, clock, _) = login();
        for _ in 0..FAILURES_BEFORE_LOCK {
            assert_eq!(
                login.sign_in(NAME, "wrong").await.unwrap_err(),
                Refusal::Refused
            );
        }
        assert_eq!(
            login.sign_in(NAME, PASSWORD).await.unwrap_err(),
            Refusal::Locked
        );
        clock.move_on(LOCK_FOR.as_secs() - 1);
        assert_eq!(
            login.sign_in(NAME, PASSWORD).await.unwrap_err(),
            Refusal::Locked
        );
        clock.move_on(2);
        assert!(login.sign_in(NAME, PASSWORD).await.is_ok());
    }

    #[tokio::test]
    async fn an_unknown_name_is_locked_too_so_the_lock_does_not_say_who_exists() {
        let (login, _, _) = login();
        for _ in 0..FAILURES_BEFORE_LOCK {
            let _ = login.sign_in("nobody@bankofmumanddad.test", "wrong").await;
        }
        assert_eq!(
            login
                .sign_in("nobody@bankofmumanddad.test", "wrong")
                .await
                .unwrap_err(),
            Refusal::Locked
        );
        // Another name is not affected.
        assert!(login.sign_in(NAME, PASSWORD).await.is_ok());
    }

    #[tokio::test]
    async fn a_right_password_starts_the_count_again() {
        let (login, _, _) = login();
        for _ in 0..FAILURES_BEFORE_LOCK - 1 {
            let _ = login.sign_in(NAME, "wrong").await;
        }
        assert!(login.sign_in(NAME, PASSWORD).await.is_ok());
        for _ in 0..FAILURES_BEFORE_LOCK - 1 {
            assert_eq!(
                login.sign_in(NAME, "wrong").await.unwrap_err(),
                Refusal::Refused
            );
        }
        assert!(login.sign_in(NAME, PASSWORD).await.is_ok());
    }

    #[tokio::test]
    async fn a_token_stops_at_its_expiry_and_when_it_is_ended() {
        let (login, clock, _) = login();
        let first = login.sign_in(NAME, PASSWORD).await.unwrap().token;
        let second = login.sign_in(NAME, PASSWORD).await.unwrap().token;
        clock.move_on(SESSION_LIFETIME.as_secs() - 1);
        assert!(login.authenticate(&first).is_some());
        login.sign_out(&second);
        assert!(login.authenticate(&second).is_none(), "ended");
        assert!(
            login.authenticate(&first).is_some(),
            "another sign-in is left alone"
        );
        clock.move_on(1);
        assert!(login.authenticate(&first).is_none(), "expired");
        assert_eq!(
            login.sessions().live(),
            0,
            "an expired token is forgotten when it is seen"
        );
        // Ending a token that is not live, or never was, does nothing.
        login.sign_out(&first);
        login.sign_out("not a token");
    }

    #[tokio::test]
    async fn a_new_list_takes_effect_for_the_next_sign_in() {
        let (login, _, users) = login();
        users.set(Arc::new(Users::empty()));
        assert_eq!(
            login.sign_in(NAME, PASSWORD).await.unwrap_err(),
            Refusal::Refused
        );
        users.set(Arc::new(
            Users::from_json(&list(&hash("a new password"))).unwrap(),
        ));
        assert!(login.sign_in(NAME, "a new password").await.is_ok());
        assert_eq!(
            login.sign_in(NAME, PASSWORD).await.unwrap_err(),
            Refusal::Refused
        );
    }

    #[tokio::test]
    async fn the_token_is_kept_only_as_a_hash() {
        let (login, _, _) = login();
        let token = login.sign_in(NAME, PASSWORD).await.unwrap().token;
        let live = login.sessions().live.lock().unwrap();
        assert!(live.keys().all(|k| k != token.as_bytes()));
        assert!(live.contains_key(&hash_token(&token)));
    }
}
