//! Where the administrator's credential is kept. The agent only ever writes it here once.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("cannot write the secret: {0}")]
    Write(String),
    #[error("cannot read the secret: {0}")]
    Read(String),
}

/// A secret store. The cloud stores come with I-01k; the file store serves customers who mount
/// their own.
pub trait SecretStore {
    fn put(&self, id: &str, value: &str) -> Result<(), StoreError>;
    fn get(&self, id: &str) -> Result<String, StoreError>;
}

/// One file for each secret, mode 0600, written to a temporary name and renamed into place.
pub struct FileStore {
    dir: PathBuf,
}

impl FileStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        FileStore { dir: dir.into() }
    }

    /// An id such as an ARN is turned into a safe file name: anything but letters, digits, dot,
    /// hyphen and underscore becomes an underscore.
    pub fn file_name(id: &str) -> String {
        id.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    }
}

impl SecretStore for FileStore {
    fn put(&self, id: &str, value: &str) -> Result<(), StoreError> {
        let err = |e: std::io::Error| StoreError::Write(e.to_string());
        fs::create_dir_all(&self.dir).map_err(err)?;
        let name = Self::file_name(id);
        let temp = self.dir.join(format!(".{name}.tmp"));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temp)
            .map_err(err)?;
        file.write_all(value.as_bytes()).map_err(err)?;
        file.sync_all().map_err(err)?;
        fs::rename(&temp, self.dir.join(name)).map_err(err)
    }

    fn get(&self, id: &str) -> Result<String, StoreError> {
        let path = self.dir.join(Self::file_name(id));
        let mode = fs::metadata(&path)
            .map_err(|e| StoreError::Read(e.to_string()))?
            .permissions()
            .mode();
        if mode & 0o077 != 0 {
            return Err(StoreError::Read(format!(
                "{} is readable by others",
                path.display()
            )));
        }
        fs::read_to_string(path).map_err(|e| StoreError::Read(e.to_string()))
    }
}
