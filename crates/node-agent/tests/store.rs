use std::os::unix::fs::PermissionsExt;

use node_agent::store::{FileStore, SecretStore};

fn temp(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("node-agent-store-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn a_secret_is_written_privately_and_read_back() {
    let dir = temp("rw");
    let store = FileStore::new(&dir);
    store
        .put(
            "arn:aws:secretsmanager:eu-west-2:1:secret:cell/admin",
            "s3cret",
        )
        .unwrap();
    let name = FileStore::file_name("arn:aws:secretsmanager:eu-west-2:1:secret:cell/admin");
    assert_eq!(name, "arn_aws_secretsmanager_eu-west-2_1_secret_cell_admin");
    let mode = std::fs::metadata(dir.join(&name))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
    assert_eq!(
        store
            .get("arn:aws:secretsmanager:eu-west-2:1:secret:cell/admin")
            .unwrap(),
        "s3cret"
    );
    // No temporary file is left behind.
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
}

#[test]
fn an_id_cannot_escape_the_directory() {
    assert_eq!(FileStore::file_name("../../etc/passwd"), ".._.._etc_passwd");
    assert!(!FileStore::file_name("a/b").contains('/'));
}

#[test]
fn a_secret_others_can_read_is_not_returned() {
    let dir = temp("mode");
    let store = FileStore::new(&dir);
    store.put("x", "v").unwrap();
    std::fs::set_permissions(dir.join("x"), std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(store.get("x").is_err());
}

#[test]
fn putting_again_replaces_the_value() {
    let dir = temp("again");
    let store = FileStore::new(&dir);
    store.put("x", "one").unwrap();
    store.put("x", "two").unwrap();
    assert_eq!(store.get("x").unwrap(), "two");
}
