//! Tests that run against the local environment from `compose.yaml` (`make up`).
//!
//! Tests that need the environment are marked `#[ignore]`, so a plain `cargo test` passes
//! without Docker. Run them with `make test-integration`; CI does the same.
