//! Errors as problem details (RFC 9457, `application/problem+json`), with a stable `code`.
//! The codes are in `docs/api-design.md#errors-idempotency-and-retries`, and `wrong_home` and
//! `unauthenticated` are added to them for this service.
//!
//! A client never sees an epoch error: a stale epoch, a lost lease and an unreachable control plane
//! are all `home_unavailable`, which is safe to retry.

use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use control_plane::{ControlError, GuardError};
use ledger_store::LegError;
use serde::Serialize;

/// A refusal, ready to send.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    title: &'static str,
    detail: String,
    retryable: bool,
    home: Option<String>,
}

#[derive(Serialize)]
struct Problem<'a> {
    r#type: String,
    title: &'a str,
    status: u16,
    code: &'a str,
    detail: &'a str,
    retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    home: Option<&'a str>,
}

impl ApiError {
    fn new(
        status: StatusCode,
        code: &'static str,
        title: &'static str,
        detail: impl Into<String>,
        retryable: bool,
    ) -> Self {
        ApiError {
            status,
            code,
            title,
            detail: detail.into(),
            retryable,
            home: None,
        }
    }

    pub fn invalid(detail: impl Into<String>) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "invalid_payload",
            "The request is not valid",
            detail,
            false,
        )
    }

    /// A sign-in that did not work. The same answer for an unknown user and a wrong password.
    pub fn invalid_credentials() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "invalid_credentials",
            "The username or password is not right",
            "Check them and try again.",
            false,
        )
    }

    /// Too many failed sign-ins for a username in a row.
    pub fn too_many_attempts() -> Self {
        Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_attempts",
            "Too many failed sign-ins",
            "Wait a minute and try again.",
            true,
        )
    }

    /// Sign-in is not set up for this cell.
    pub fn sign_in_not_available() -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "sign_in_not_available",
            "Sign-in is not set up on this cell",
            "Use a bearer token.",
            false,
        )
    }

    pub fn unauthenticated() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "unauthenticated",
            "A valid bearer token is needed",
            "send Authorization: Bearer <token>",
            false,
        )
    }

    pub fn not_found() -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "Not found",
            "no such account for this tenant",
            false,
        )
    }

    pub fn idempotency_conflict() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "idempotency_conflict",
            "The idempotency key was used with a different request",
            "use a new key for a different request",
            false,
        )
    }

    pub fn insufficient_funds(detail: impl Into<String>) -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "insufficient_funds",
            "Available balance is too low",
            detail,
            false,
        )
    }

    pub fn home_unavailable(detail: impl Into<String>) -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "home_unavailable",
            "The account's home cell cannot take this now",
            detail,
            true,
        )
    }

    /// The account is homed at another cell. The caller should ask `home`.
    pub fn wrong_home(home: &str) -> Self {
        let mut e = Self::new(
            StatusCode::MISDIRECTED_REQUEST,
            "wrong_home",
            "This cell is not home for the account",
            "send the request to the home cell",
            false,
        );
        e.home = Some(home.to_owned());
        e
    }

    pub fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "Something went wrong here",
            "retrying is safe only with the same idempotency key",
            false,
        )
    }

    pub fn code(&self) -> &'static str {
        self.code
    }

    pub fn status(&self) -> StatusCode {
        self.status
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Problem {
            r#type: format!("urn:ledger:problem:{}", self.code),
            title: self.title,
            status: self.status.as_u16(),
            code: self.code,
            detail: &self.detail,
            retryable: self.retryable,
            home: self.home.as_deref(),
        };
        let mut response = (self.status, Json(body)).into_response();
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        if self.status == StatusCode::SERVICE_UNAVAILABLE {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
        }
        response
    }
}

impl From<LegError> for ApiError {
    fn from(e: LegError) -> Self {
        match e {
            LegError::NotPositive => ApiError::invalid("the amount must be positive"),
            LegError::InsufficientFunds(d) => ApiError::insufficient_funds(d),
            // A stale epoch is the API's business to handle, never the client's.
            LegError::Fenced(d) => ApiError::home_unavailable(d),
            LegError::IdempotencyConflict(_) => ApiError::idempotency_conflict(),
            LegError::UnknownAccount(_) | LegError::UnknownHold(_) => ApiError::not_found(),
            LegError::CurrencyMismatch(d) => ApiError::invalid(d),
            LegError::HoldState(d) => ApiError::invalid(d),
            // The details of a database failure never reach the caller.
            LegError::BadId(_) | LegError::Schema(_) | LegError::Database(_) => {
                // Kept here, in the service's own log, and never sent to the caller.
                eprintln!("ledger-api: store error: {e:?}");
                ApiError::internal()
            }
        }
    }
}

impl From<GuardError> for ApiError {
    fn from(e: GuardError) -> Self {
        match e {
            GuardError::NotHome { home, .. } => ApiError::wrong_home(home.as_str()),
            GuardError::LeaseLost { .. } => {
                ApiError::home_unavailable("the lease for the account has lapsed")
            }
            GuardError::Unavailable { .. } => {
                ApiError::home_unavailable("the control plane cannot be reached")
            }
            GuardError::Refused(ControlError::UnknownAccount(_)) => ApiError::not_found(),
            GuardError::Refused(_) => {
                ApiError::home_unavailable("the control plane refused the check")
            }
        }
    }
}

impl From<ControlError> for ApiError {
    fn from(e: ControlError) -> Self {
        match e {
            ControlError::UnknownAccount(_) => ApiError::not_found(),
            ControlError::NotHome { home, .. } => ApiError::wrong_home(home.as_str()),
            _ => ApiError::home_unavailable("the control plane cannot take this now"),
        }
    }
}
