//! K-12: the service and `api/openapi.yaml` must agree. Every route the service answers is in the
//! file, and every operation the file marks `x-skeleton: true` is one the service answers.
//! No database needed.

use std::collections::BTreeSet;

fn spec() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../api/openapi.yaml");
    std::fs::read_to_string(path).expect("api/openapi.yaml")
}

/// The (method, path, skeleton) of every operation under `paths:`. The file is regular: a path
/// is indented two spaces, a method four, and a field of an operation six.
fn operations(text: &str) -> Vec<(String, String, bool)> {
    let mut out: Vec<(String, String, bool)> = Vec::new();
    let mut in_paths = false;
    let mut path = String::new();
    for line in text.lines() {
        if !line.starts_with(' ') && !line.trim().is_empty() && !line.starts_with('#') {
            in_paths = line.starts_with("paths:");
            continue;
        }
        if !in_paths {
            continue;
        }
        if let Some(p) = line.strip_prefix("  /") {
            path = format!("/{}", p.trim_end_matches(':'));
        } else if let Some(m) = line.strip_prefix("    ") {
            let method = m.trim_end_matches(':');
            if !m.starts_with(' ')
                && m.ends_with(':')
                && ["get", "post", "put", "patch", "delete"].contains(&method)
            {
                out.push((method.to_owned(), path.clone(), false));
            }
        }
        if line.trim() == "x-skeleton: true"
            && let Some(last) = out.last_mut()
        {
            last.2 = true;
        }
    }
    out
}

#[test]
fn every_route_the_service_answers_is_in_the_file() {
    let file: BTreeSet<(String, String)> = operations(&spec())
        .into_iter()
        .map(|(m, p, _)| (m, p))
        .collect();
    for (method, path) in ledger_api::ROUTES {
        assert!(
            file.contains(&((*method).to_owned(), (*path).to_owned())),
            "{method} {path} is served but not in api/openapi.yaml"
        );
    }
}

#[test]
fn every_skeleton_operation_in_the_file_is_served() {
    let served: BTreeSet<(String, String)> = ledger_api::ROUTES
        .iter()
        .map(|(m, p)| ((*m).to_owned(), (*p).to_owned()))
        .collect();
    let marked: Vec<_> = operations(&spec())
        .into_iter()
        .filter(|(_, _, skeleton)| *skeleton)
        .collect();
    assert!(!marked.is_empty(), "the file marks no skeleton operation");
    for (method, path, _) in marked {
        assert!(
            served.contains(&(method.clone(), path.clone())),
            "{method} {path} is marked x-skeleton but the service does not answer it"
        );
    }
}

#[test]
fn the_file_has_no_way_to_change_or_remove_an_entry() {
    for (method, path, _) in operations(&spec()) {
        assert!(
            !["put", "patch", "delete"].contains(&method.as_str()),
            "{method} {path} would write a resource directly"
        );
    }
}
