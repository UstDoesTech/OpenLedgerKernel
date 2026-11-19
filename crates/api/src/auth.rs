//! Who is calling. Every request carries a bearer token, and the token names the tenant. The
//! tenant is never read from a path or a body.
//!
//! [`Authenticator`] is the contract. [`StaticTokens`] is the stand-in for tests and the proving
//! environment: a fixed list of tokens, kept as hashes so a memory dump or a log of the map shows
//! none. Real sign-in (workforce single sign-on, device-bound customer tokens, certificate-bound
//! agent tokens) is task A-06 and replaces it behind the same trait.
//! See `docs/api-design.md#identity-and-authorisation`.

use std::collections::HashMap;

use ledger_core::TenantId;
use sha2::{Digest, Sha256};

/// Who is calling, as the token says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub tenant: TenantId,
    /// Who is accountable, such as `usr_ada` or `agt_rates`. It is written into journals.
    pub name: String,
}

/// Turns a bearer token into a principal, or says it is not one.
pub trait Authenticator: Send + Sync {
    fn authenticate(&self, token: &str) -> Option<Principal>;
}

/// A fixed list of tokens. For tests and the proving environment only.
#[derive(Debug, Default)]
pub struct StaticTokens {
    by_hash: HashMap<[u8; 32], Principal>,
}

fn hash(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

/// Why a token list was refused.
#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    #[error("a token is at least 32 characters, so it cannot be guessed")]
    TooShort,
    #[error("the token list is not valid: {0}")]
    Invalid(String),
}

impl StaticTokens {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a token for a tenant and a principal name (`usr_`, `agt_` or `sys_` and lower-case
    /// letters or digits).
    pub fn with(mut self, token: &str, tenant: TenantId, name: &str) -> Result<Self, TokenError> {
        if token.len() < 32 {
            return Err(TokenError::TooShort);
        }
        let ok = ["usr_", "agt_", "sys_"].iter().any(|p| {
            name.strip_prefix(p).is_some_and(|b| {
                !b.is_empty()
                    && b.len() <= 64
                    && b.bytes()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            })
        });
        if !ok {
            return Err(TokenError::Invalid(format!(
                "{name:?} is not a principal name"
            )));
        }
        self.by_hash.insert(
            hash(token),
            Principal {
                tenant,
                name: name.to_owned(),
            },
        );
        Ok(self)
    }

    /// Reads `[{"token": "...", "tenant": "ten_x", "principal": "usr_ada"}, ...]`.
    pub fn from_json(text: &str) -> Result<Self, TokenError> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Entry {
            token: String,
            tenant: TenantId,
            principal: String,
        }
        let entries: Vec<Entry> =
            serde_json::from_str(text).map_err(|e| TokenError::Invalid(e.to_string()))?;
        let mut tokens = StaticTokens::new();
        for e in entries {
            tokens = tokens.with(&e.token, e.tenant, &e.principal)?;
        }
        Ok(tokens)
    }
}

impl Authenticator for StaticTokens {
    fn authenticate(&self, token: &str) -> Option<Principal> {
        self.by_hash.get(&hash(token)).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const TOKEN_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn tokens() -> StaticTokens {
        StaticTokens::new()
            .with(TOKEN_A, "ten_acme".parse().unwrap(), "usr_ada")
            .unwrap()
            .with(TOKEN_B, "ten_zed".parse().unwrap(), "agt_rates")
            .unwrap()
    }

    #[test]
    fn a_token_names_its_tenant_and_nobody_else() {
        let t = tokens();
        assert_eq!(t.authenticate(TOKEN_A).unwrap().tenant.as_str(), "ten_acme");
        assert_eq!(t.authenticate(TOKEN_B).unwrap().tenant.as_str(), "ten_zed");
        assert!(
            t.authenticate("cccccccccccccccccccccccccccccccccccccccc")
                .is_none()
        );
        assert!(t.authenticate("").is_none());
    }

    #[test]
    fn the_list_holds_hashes_not_tokens() {
        let shown = format!("{:?}", tokens());
        assert!(!shown.contains(TOKEN_A) && !shown.contains(TOKEN_B));
    }

    #[test]
    fn short_tokens_and_odd_names_are_refused() {
        let t = StaticTokens::new();
        assert!(matches!(
            t.with("short", "ten_acme".parse().unwrap(), "usr_ada"),
            Err(TokenError::TooShort)
        ));
        let t = StaticTokens::new();
        assert!(t.with(TOKEN_A, "ten_acme".parse().unwrap(), "ada").is_err());
    }

    #[test]
    fn the_json_list_loads_and_refuses_extra_fields() {
        let ok = format!(r#"[{{"token":"{TOKEN_A}","tenant":"ten_acme","principal":"usr_ada"}}]"#);
        assert!(
            StaticTokens::from_json(&ok)
                .unwrap()
                .authenticate(TOKEN_A)
                .is_some()
        );
        let extra = format!(
            r#"[{{"token":"{TOKEN_A}","tenant":"ten_acme","principal":"usr_ada","admin":true}}]"#
        );
        assert!(StaticTokens::from_json(&extra).is_err());
    }
}
