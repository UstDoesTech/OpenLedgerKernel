//! The standby list the agent builds must be one PostgreSQL accepts. This runs the real server
//! binary, because a unit test of the text alone once passed a value PostgreSQL refused.

use std::path::{Path, PathBuf};
use std::process::Command;

use node_agent::plan;

fn bin() -> Option<PathBuf> {
    let found = std::fs::read_dir("/usr/lib/postgresql").ok()?;
    let mut versions: Vec<PathBuf> = found.flatten().map(|e| e.path().join("bin")).collect();
    versions.sort();
    versions
        .into_iter()
        .rev()
        .find(|b| b.join("postgres").exists() && b.join("initdb").exists())
}

fn is_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .map(|o| o.stdout == b"0\n")
        .unwrap_or(false)
}

fn value_as_postgres_reads_it(bin: &Path, data: &Path, setting: &str) -> (bool, String) {
    let out = Command::new(bin.join("postgres"))
        .arg("-D")
        .arg(data)
        .arg("-c")
        .arg(format!("synchronous_standby_names={setting}"))
        .args(["-C", "synchronous_standby_names"])
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

#[test]
fn postgres_accepts_the_standby_list_with_hyphenated_names_and_refuses_it_unquoted() {
    let Some(bin) = bin() else {
        eprintln!("skipped: no PostgreSQL server binary here");
        return;
    };
    if is_root() {
        eprintln!("skipped: PostgreSQL will not run as root");
        return;
    }
    let dir = std::env::temp_dir().join(format!("standby-names-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let init = Command::new(bin.join("initdb"))
        .arg("-D")
        .arg(&dir)
        .output()
        .unwrap();
    assert!(init.status.success(), "initdb failed");

    let configured = "ANY 1 (cell-a-n2, cell-a-n3)";
    let (accepted, text) = value_as_postgres_reads_it(&bin, &dir, &plan::standby_list(configured));
    assert!(accepted, "{text}");
    // The text it was refused in before: proves this test would have caught the fault.
    let (accepted, _) = value_as_postgres_reads_it(&bin, &dir, configured);
    assert!(!accepted);
    let _ = std::fs::remove_dir_all(&dir);
}
