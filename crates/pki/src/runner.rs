//! Running the cloud's own command line tools. Their output holds identifiers and sometimes
//! secrets, so it is returned to the caller and never printed. A failure carries the exit code and
//! nothing the command said.

use std::process::{Command, Stdio};

use crate::error::PkiError;

pub trait Runner: Send + Sync {
    /// Runs `program` with `args` and returns what it wrote to standard output.
    fn run(&self, program: &str, args: &[String], doing: &'static str)
    -> Result<Vec<u8>, PkiError>;
}

/// Runs the real command. Standard error is dropped.
#[derive(Debug, Default)]
pub struct ProcessRunner;

impl Runner for ProcessRunner {
    fn run(
        &self,
        program: &str,
        args: &[String],
        doing: &'static str,
    ) -> Result<Vec<u8>, PkiError> {
        let out = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map_err(|_| PkiError::CannotRun(doing))?;
        if out.status.success() {
            Ok(out.stdout)
        } else {
            Err(PkiError::Command(out.status.code().unwrap_or(-1), doing))
        }
    }
}
