//! The basic page (UI-01, D-20). Three files compiled into the binary and served at `/ui`, so the
//! page and the API share an origin and nothing else has to be set up. They need no token, because
//! they hold none: the page asks for a token by signing in, and keeps it in memory only.
//!
//! Every response says what the page may do (a content security policy that allows only its own
//! files), that it may not be framed, and that nothing is to be kept.

use axum::Router;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

const INDEX: &str = include_str!("../../../ui/index.html");
const SCRIPT: &str = include_str!("../../../ui/app.js");
const STYLE: &str = include_str!("../../../ui/style.css");

/// What the page may load and where it may send requests: its own origin and nothing else. It may
/// not be framed, set a base address or post a form anywhere.
pub const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// The paths the page is served at. They are not part of the API, so they are not in `ROUTES` or in
/// `api/openapi.yaml`.
pub const UI_PATHS: &[&str] = &["/ui", "/ui/", "/ui/app.js", "/ui/style.css"];

fn file(kind: &'static str, body: &'static str) -> Response {
    let mut response = (StatusCode::OK, body).into_response();
    let h = response.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(kind));
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CONTENT_SECURITY_POLICY),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn index() -> Response {
    file("text/html; charset=utf-8", INDEX)
}

async fn script() -> Response {
    file("text/javascript; charset=utf-8", SCRIPT)
}

async fn style() -> Response {
    file("text/css; charset=utf-8", STYLE)
}

/// The page's routes.
pub fn routes() -> Router {
    Router::new()
        .route("/ui", get(index))
        .route("/ui/", get(index))
        .route("/ui/app.js", get(script))
        .route("/ui/style.css", get(style))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn get_path(path: &str) -> (StatusCode, axum::http::HeaderMap, String) {
        let response = routes()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let (parts, body) = response.into_parts();
        let bytes = body.collect().await.unwrap().to_bytes();
        (
            parts.status,
            parts.headers,
            String::from_utf8(bytes.to_vec()).unwrap(),
        )
    }

    #[tokio::test]
    async fn every_file_is_served_without_a_token_and_with_the_right_type() {
        for (path, kind) in [
            ("/ui", "text/html"),
            ("/ui/", "text/html"),
            ("/ui/app.js", "text/javascript"),
            ("/ui/style.css", "text/css"),
        ] {
            let (status, headers, body) = get_path(path).await;
            assert_eq!(status, StatusCode::OK, "{path}");
            assert!(
                headers["content-type"].to_str().unwrap().starts_with(kind),
                "{path}"
            );
            assert!(!body.is_empty(), "{path}");
        }
    }

    #[tokio::test]
    async fn every_response_has_the_policy_no_framing_and_no_caching() {
        for path in UI_PATHS {
            let (_, headers, _) = get_path(path).await;
            assert_eq!(headers["content-security-policy"], CONTENT_SECURITY_POLICY);
            assert_eq!(headers["x-frame-options"], "DENY");
            assert_eq!(headers["x-content-type-options"], "nosniff");
            assert_eq!(headers["referrer-policy"], "no-referrer");
            assert_eq!(headers["cache-control"], "no-store", "{path}");
        }
    }

    #[test]
    fn the_policy_allows_only_the_pages_own_files_and_no_framing() {
        let policy = CONTENT_SECURITY_POLICY;
        assert!(policy.starts_with("default-src 'none'"));
        assert!(policy.contains("script-src 'self'"));
        assert!(policy.contains("frame-ancestors 'none'"));
        for loosened in [
            "unsafe-inline",
            "unsafe-eval",
            "http:",
            "https:",
            "data:",
            "*",
        ] {
            assert!(!policy.contains(loosened), "{loosened}");
        }
    }

    #[tokio::test]
    async fn anything_else_under_ui_is_not_found() {
        for path in ["/ui/other.js", "/ui/../Cargo.toml", "/ui/app.js/x"] {
            let (status, _, _) = get_path(path).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        }
    }

    #[test]
    fn the_page_has_a_language_a_title_a_heading_a_main_region_and_labelled_controls() {
        assert!(INDEX.contains("<html lang=\"en-GB\">"));
        assert!(INDEX.contains("<title>"));
        assert!(INDEX.contains("<main>"));
        assert_eq!(INDEX.matches("<h1>").count(), 1);
        for id in ["username", "password"] {
            assert!(INDEX.contains(&format!("<label for=\"{id}\">")), "{id}");
            assert!(INDEX.contains(&format!("<input id=\"{id}\"")), "{id}");
        }
        assert!(INDEX.contains("autocomplete=\"current-password\""));
    }

    #[test]
    fn the_page_loads_only_its_own_files_and_has_no_inline_script_or_style() {
        for (name, text) in [
            ("index.html", INDEX),
            ("app.js", SCRIPT),
            ("style.css", STYLE),
        ] {
            assert!(!text.contains("http://"), "{name}");
            assert!(!text.contains("https://"), "{name}");
            assert!(!text.contains("//cdn"), "{name}");
        }
        assert!(INDEX.contains("<script type=\"module\" src=\"/ui/app.js\">"));
        assert!(INDEX.contains("<link rel=\"stylesheet\" href=\"/ui/style.css\">"));
        assert!(!INDEX.contains("<style"));
        assert!(!INDEX.contains(" style="));
        assert!(!INDEX.contains(" onclick="));
        assert!(!INDEX.contains("<script>"));
    }

    #[test]
    fn the_token_is_kept_in_memory_only_and_never_logged_or_put_in_a_url() {
        for stored in [
            "localStorage",
            "sessionStorage",
            "document.cookie",
            "indexedDB",
            "console.",
            "location.",
            "history.",
            "innerHTML",
            "eval(",
        ] {
            assert!(!SCRIPT.contains(stored), "app.js must not use {stored}");
        }
        assert!(SCRIPT.contains("session.token = issued.token"));
        // Where the token is sent: a header, never a query string.
        assert!(SCRIPT.contains("authorization: `Bearer ${session.token}`"));
        assert!(!SCRIPT.contains("?token"));
    }

    #[test]
    fn money_is_whole_minor_units_and_a_currency_and_never_a_decimal() {
        for decimal in [
            "parseFloat",
            "toFixed",
            "toLocale",
            "NumberFormat",
            "Math.round",
            "Math.floor",
        ] {
            assert!(!SCRIPT.contains(decimal), "app.js must not use {decimal}");
        }
        // No scaling by a hundred, which is how pounds and pence get mixed up. (`* 1000` for a timer
        // is not that, so only a standalone 100 counts.)
        for op in ["/ 100", "* 100", "/100", "*100"] {
            for (at, _) in SCRIPT.match_indices(op) {
                let next = SCRIPT[at + op.len()..].chars().next().unwrap_or(' ');
                assert!(next.is_ascii_digit(), "app.js scales by 100 with {op}");
            }
        }
        // The only way an amount is shown, and an amount that is not exact is refused.
        assert!(SCRIPT.contains("`${value.minor} ${value.currency}`"));
        assert!(SCRIPT.contains("Number.isSafeInteger(value.minor)"));
        // An amount typed in is digits only.
        assert!(SCRIPT.contains("/^[0-9]{1,15}$/"));
    }

    #[test]
    fn no_failure_shows_what_the_server_said() {
        // The server's title, detail and type can quote an identifier, so none is ever read.
        for read in [
            ".detail",
            ".title",
            ".type",
            ".code",
            "[\"detail\"]",
            "[\"title\"]",
        ] {
            assert!(
                !SCRIPT.contains(read),
                "app.js must not read {read} from an error"
            );
        }
        // Each failure is said from the status alone.
        assert!(SCRIPT.contains("function say(status)"));
        assert!(SCRIPT.contains("say(result.status)"));
        // Values from the server are put on the page as text, never as markup.
        assert!(!SCRIPT.contains("innerHTML"));
        assert!(!SCRIPT.contains("insertAdjacentHTML"));
        assert!(!SCRIPT.contains("document.write"));
    }

    #[test]
    fn every_input_has_a_label_and_every_hint_is_tied_to_its_input() {
        let mut inputs = 0;
        for piece in INDEX.split("<input id=\"").skip(1) {
            let id = piece.split('"').next().unwrap();
            inputs += 1;
            assert!(
                INDEX.contains(&format!("<label for=\"{id}\">")),
                "{id} has no label"
            );
        }
        assert_eq!(
            inputs, 5,
            "username, password, currency, overdraft, account"
        );
        for hint in ["currency-hint", "overdraft-hint", "account-hint"] {
            assert!(
                INDEX.contains(&format!("aria-describedby=\"{hint}\"")),
                "{hint}"
            );
            assert!(INDEX.contains(&format!("<p id=\"{hint}\"")), "{hint}");
        }
        assert!(INDEX.contains("<caption>"));
        assert!(INDEX.contains("scope=\"col\""));
        assert!(INDEX.contains("scope=\"row\""));
    }

    #[test]
    fn every_element_the_script_looks_for_is_in_the_page() {
        let mut checked = 0;
        for piece in SCRIPT.split("$(\"").skip(1) {
            let id = piece.split('"').next().unwrap();
            assert!(
                INDEX.contains(&format!("id=\"{id}\"")),
                "app.js looks for #{id}"
            );
            checked += 1;
        }
        assert!(checked > 20);
    }

    #[test]
    fn opening_an_account_sends_an_idempotency_key_and_keeps_it_for_a_repeat_of_the_same_form() {
        assert!(SCRIPT.contains("\"idempotency-key\""));
        assert!(SCRIPT.contains("crypto.randomUUID()"));
        assert!(SCRIPT.contains("pending.form !== form"));
    }
}
