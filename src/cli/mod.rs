//! Reusable implementations of the commands exposed by the `oli` binary.
//!
//! Clap syntax stays in `src/bin/oli.rs`; these modules own command behavior and
//! top-level runtime assembly.

pub mod auth;
pub mod init;
pub mod mcp;
pub mod replay;
pub mod run;
pub mod sessions;
