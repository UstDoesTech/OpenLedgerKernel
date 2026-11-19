//! K-14: `api/internal.md` and the internal listener must agree. Every internal route is described,
//! and every route the file describes is served. No database needed.

use std::collections::BTreeSet;

fn documented() -> BTreeSet<(String, String)> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../api/internal.md");
    let text = std::fs::read_to_string(path).expect("api/internal.md");
    text.lines()
        .filter_map(|l| l.strip_prefix("### `"))
        .filter_map(|l| l.strip_suffix('`'))
        .filter_map(|l| l.split_once(' '))
        .map(|(m, p)| (m.to_lowercase(), p.to_owned()))
        .collect()
}

#[test]
fn the_file_and_the_listener_describe_the_same_routes() {
    let served: BTreeSet<(String, String)> = ledger_api::INTERNAL_ROUTES
        .iter()
        .map(|(m, p)| ((*m).to_owned(), (*p).to_owned()))
        .collect();
    assert!(!served.is_empty());
    assert_eq!(documented(), served);
}

#[test]
fn no_internal_route_is_in_the_public_api_file() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../api/openapi.yaml");
    let spec = std::fs::read_to_string(path).unwrap();
    assert!(!spec.contains("/internal/"));
}
