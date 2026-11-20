//! Re-reading files that rotate. Certificates, tokens and the list of cells all change while the
//! service runs, and a rotation must not need a restart or drop a request.
//!
//! [`Reloadable`] holds the current value and hands out a snapshot. Whoever is mid-request keeps
//! the snapshot they took, so a swap never changes a value under a request. [`watch`] polls a set of
//! files and calls a loader when their contents change. A loader that fails leaves the old value
//! in force and is tried again at the next poll, so a half-written set of files costs nothing.
//! Nothing here prints a file's content, only counts.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::auth::{Authenticator, Peer, PeerAuthenticator, PeerTokens, Principal, StaticTokens};

/// A value that can be swapped while it is in use.
pub struct Reloadable<T: ?Sized> {
    current: Arc<RwLock<Arc<T>>>,
}

impl<T: ?Sized> Clone for Reloadable<T> {
    fn clone(&self) -> Self {
        Reloadable {
            current: self.current.clone(),
        }
    }
}

impl<T: ?Sized> std::fmt::Debug for Reloadable<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // What is inside may be a credential list, so it is never shown.
        f.write_str("Reloadable(..)")
    }
}

impl<T> Reloadable<T> {
    pub fn new(value: T) -> Self {
        Self::from_arc(Arc::new(value))
    }
}

impl<T: ?Sized> Reloadable<T> {
    pub fn from_arc(value: Arc<T>) -> Self {
        Reloadable {
            current: Arc::new(RwLock::new(value)),
        }
    }

    /// The value now in force. It stays valid for as long as the caller holds it.
    pub fn get(&self) -> Arc<T> {
        self.current
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Puts a new value in force. Calls already holding the old one finish with it.
    pub fn set(&self, value: Arc<T>) {
        *self.current.write().unwrap_or_else(|e| e.into_inner()) = value;
    }
}

impl Authenticator for Reloadable<StaticTokens> {
    fn authenticate(&self, token: &str) -> Option<Principal> {
        self.get().authenticate(token)
    }
}

impl PeerAuthenticator for Reloadable<PeerTokens> {
    fn authenticate(&self, token: &str) -> Option<Peer> {
        self.get().authenticate(token)
    }
}

/// A fingerprint of the content of some files. A file that cannot be read counts as a change, so
/// a file that appears or goes is noticed.
pub fn fingerprint(files: &[PathBuf]) -> [u8; 32] {
    let mut h = Sha256::new();
    for f in files {
        match std::fs::read(f) {
            Ok(bytes) => {
                h.update([1]);
                h.update((bytes.len() as u64).to_le_bytes());
                h.update(bytes);
            }
            Err(_) => h.update([0]),
        }
    }
    h.finalize().into()
}

/// What one pass of [`check`] found.
#[derive(Debug, PartialEq, Eq)]
pub enum Pass {
    Unchanged,
    Reloaded,
    /// The files changed and could not be used. The old values stay in force.
    Kept,
}

/// Remembers what was last seen, so a pass says whether anything changed.
pub struct Watch {
    files: Vec<PathBuf>,
    loaded: [u8; 32],
    failed: Option<[u8; 32]>,
}

impl Watch {
    /// Starts from the files as they are now, which the caller has just loaded.
    pub fn new(files: Vec<PathBuf>) -> Self {
        let loaded = fingerprint(&files);
        Watch {
            files,
            loaded,
            failed: None,
        }
    }

    /// Calls `load` if the files changed since the last good load. `load` returns whether the new
    /// content was usable; a reason is never passed on, because it could quote a file.
    pub fn check(&mut self, load: impl FnOnce() -> bool) -> Pass {
        let now = fingerprint(&self.files);
        if now == self.loaded {
            return Pass::Unchanged;
        }
        if self.failed == Some(now) {
            // Already refused this exact content. Try again only when it changes again.
            return Pass::Kept;
        }
        if load() {
            self.loaded = now;
            self.failed = None;
            Pass::Reloaded
        } else {
            self.failed = Some(now);
            Pass::Kept
        }
    }
}

/// Polls `files` every `every`, forever, and calls `load` when they change. It prints one line,
/// with no content, each time it reloads or keeps the old values.
pub fn spawn_watch(
    name: &'static str,
    files: Vec<PathBuf>,
    every: Duration,
    mut load: impl FnMut() -> bool + Send + 'static,
) {
    tokio::spawn(async move {
        let mut watch = Watch::new(files);
        let mut tick = tokio::time::interval(every);
        tick.tick().await;
        loop {
            tick.tick().await;
            match watch.check(&mut load) {
                Pass::Unchanged => {}
                Pass::Reloaded => eprintln!("ledger-api: reloaded {name}"),
                Pass::Kept => eprintln!(
                    "ledger-api: {name} changed and was refused; the old ones stay in force"
                ),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_swap_does_not_change_a_snapshot_already_taken() {
        let r = Reloadable::new(1);
        let before = r.get();
        r.set(Arc::new(2));
        assert_eq!((*before, *r.get()), (1, 2));
    }

    #[test]
    fn a_pass_reloads_on_change_and_keeps_old_values_when_the_new_ones_fail() {
        let dir = tempdir();
        let f = dir.join("a");
        std::fs::write(&f, "one").unwrap();
        let mut w = Watch::new(vec![f.clone()]);
        assert_eq!(w.check(|| panic!("nothing changed")), Pass::Unchanged);

        std::fs::write(&f, "two").unwrap();
        assert_eq!(w.check(|| false), Pass::Kept);
        // The same refused content is not tried again.
        assert_eq!(w.check(|| panic!("already refused")), Pass::Kept);

        std::fs::write(&f, "three").unwrap();
        assert_eq!(w.check(|| true), Pass::Reloaded);
        assert_eq!(w.check(|| panic!("nothing changed")), Pass::Unchanged);
    }

    #[test]
    fn a_file_that_goes_is_a_change() {
        let dir = tempdir();
        let f = dir.join("a");
        std::fs::write(&f, "one").unwrap();
        let mut w = Watch::new(vec![f.clone()]);
        std::fs::remove_file(&f).unwrap();
        assert_eq!(w.check(|| false), Pass::Kept);
    }

    #[test]
    fn debug_shows_nothing_inside() {
        let r = Reloadable::new("secret-value".to_owned());
        assert!(!format!("{r:?}").contains("secret-value"));
    }

    fn tempdir() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "reload-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}
