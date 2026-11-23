//! Errors. None of them carries a cell, a secret reference, a key id or any content: the job's log
//! is public, and a message that names one would put it there.

#[derive(Debug, thiserror::Error)]
pub enum PkiError {
    #[error("the manifest is not usable: {0}")]
    Manifest(&'static str),
    #[error("a command failed (exit code {code}) while {doing}: {tool}, {cause}")]
    Command {
        code: i32,
        doing: &'static str,
        /// One of a fixed few words: aws, az, gcloud or a tool.
        tool: &'static str,
        /// One of a fixed few phrases chosen by `runner::cause`. Never the tool's own words.
        cause: &'static str,
    },
    #[error("a command could not be run while {0}")]
    CannotRun(&'static str),
    #[error("an answer could not be read while {0}")]
    Unreadable(&'static str),
    #[error("the signer failed: {0}")]
    Signer(&'static str),
    #[error("a certificate could not be made: {0}")]
    Issuing(&'static str),
    #[error("a secret could not be {0}")]
    Store(&'static str),
    #[error(
        "the run stopped at step {step} because {why}. What was written before it still works. Run it again"
    )]
    Stopped { step: u8, why: &'static str },
}
