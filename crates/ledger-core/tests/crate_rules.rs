//! Rules about the crate itself: no floating point, no I/O dependencies.

use std::fs;
use std::path::Path;

fn sources() -> Vec<(String, String)> {
    fn walk(dir: &Path, out: &mut Vec<(String, String)>) {
        for e in fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push((p.display().to_string(), fs::read_to_string(&p).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut out);
    out
}

#[test]
fn no_floating_point_types_in_the_crate() {
    // Built from parts so this file does not match itself.
    let banned = [format!("f{}", 32), format!("f{}", 64)];
    for (path, text) in sources() {
        for line in text.lines() {
            let code = line.split("//").next().unwrap_or("");
            for b in &banned {
                let found = code.match_indices(b.as_str()).any(|(i, _)| {
                    let before = code[..i].chars().next_back();
                    let after = code[i + b.len()..].chars().next();
                    !before.is_some_and(|c| c.is_alphanumeric() || c == '_')
                        && !after.is_some_and(|c| c.is_alphanumeric() || c == '_')
                });
                assert!(!found, "{path}: floating point type in {line:?}");
            }
        }
    }
}

#[test]
fn only_approved_dependencies() {
    // Allow-list of pure-computation dependencies. Anything that does I/O (network, files,
    // databases, async runtimes, clocks) is not on it. Adding one needs a person's review.
    let allowed = ["serde", "thiserror"];
    let manifest =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).unwrap();
    let mut in_deps = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_deps = line == "[dependencies]";
            continue;
        }
        if in_deps && !line.is_empty() && !line.starts_with('#') {
            let name = line.split(['=', ' ']).next().unwrap();
            assert!(
                allowed.contains(&name),
                "dependency {name} is not approved for ledger-core"
            );
        }
    }
}
