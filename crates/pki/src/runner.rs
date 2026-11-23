//! Running the cloud's own command line tools. Their output holds identifiers and sometimes
//! secrets, so it is returned to the caller and never printed. A failure carries the exit code, the
//! tool's name and one cause picked from a fixed list. Nothing the command said is kept: its words
//! are only compared with the list.

use std::process::{Command, Stdio};

use crate::error::PkiError;

pub trait Runner: Send + Sync {
    /// Runs `program` with `args` and returns what it wrote to standard output.
    fn run(&self, program: &str, args: &[String], doing: &'static str)
    -> Result<Vec<u8>, PkiError>;
}

/// The cause of a failure, from a fixed list, chosen by looking for known marks in what the tool said
/// on standard error. The text itself is never returned, so a name, an address or a secret in it
/// cannot reach the log. The firewall mark comes first, because Azure also says "forbidden" then.
pub fn cause(said: &str) -> &'static str {
    let s = said.to_ascii_lowercase();
    let has = |marks: &[&str]| marks.iter().any(|m| s.contains(m));
    if has(&[
        "forbiddenbyfirewall",
        "firewall",
        "client address is not authorized",
    ]) {
        "blocked by a firewall"
    } else if has(&[
        "expiredtoken",
        "unauthenticated",
        "az login",
        "invalidclienttokenid",
        "unable to locate credentials",
    ]) {
        "not signed in"
    } else if has(&[
        "accessdenied",
        "authorizationfailed",
        "permission_denied",
        "permission denied",
        "forbidden",
        "not authorized",
        "403",
    ]) {
        "permission refused"
    } else if has(&[
        "resourcenotfound",
        "secretnotfound",
        "not_found",
        "not found",
        "404",
    ]) {
        "not found"
    } else if has(&[
        "could not connect",
        "timed out",
        "name or service not known",
        "connection reset",
        "unreachable",
    ]) {
        "no network"
    } else {
        "another cause"
    }
}

fn tool_name(program: &str) -> &'static str {
    match program {
        "aws" => "aws",
        "az" => "az",
        "gcloud" => "gcloud",
        _ => "a tool",
    }
}

/// Runs the real command. Standard error is read only to choose a cause, and then dropped.
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
            .stderr(Stdio::piped())
            .output()
            .map_err(|_| PkiError::CannotRun(doing))?;
        if out.status.success() {
            Ok(out.stdout)
        } else {
            Err(PkiError::Command {
                code: out.status.code().unwrap_or(-1),
                doing,
                tool: tool_name(program),
                cause: cause(&String::from_utf8_lossy(&out.stderr)),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::cause;

    #[test]
    fn a_cause_is_picked_from_the_list_and_never_quotes_the_tool() {
        let kms = "An error occurred (AccessDeniedException) when calling the GetSecretValue operation: User: arn:aws:sts::123456789012:assumed-role/x is not authorized to perform: kms:Decrypt";
        assert_eq!(cause(kms), "permission refused");
        assert_eq!(
            cause(
                "(Forbidden) Client address is not authorized and caller is not a trusted service"
            ),
            "blocked by a firewall"
        );
        assert_eq!(
            cause("ERROR: (SecretNotFound) A secret with (name/id) x was not found"),
            "not found"
        );
        assert_eq!(
            cause("ERROR: Please run 'az login' to setup account."),
            "not signed in"
        );
        assert_eq!(
            cause("An error occurred (ResourceNotFoundException)"),
            "not found"
        );
        assert_eq!(cause("something odd"), "another cause");
        for said in [kms, "x"] {
            assert!(!cause(said).contains("123456789012"));
        }
    }
}
