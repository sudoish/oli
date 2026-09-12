//! Slash-command registry and bundled command implementations for the REPL.

mod basic;
mod config;
mod cost;
mod mcp;
mod memory;
mod paths;
mod plugins;
mod registry;
mod runtime;
mod selection;

pub use basic::{Clear, Exit, Help};
pub use config::ConfigCmd;
pub use cost::Cost;
pub use mcp::Mcp;
pub use memory::{Compact, MemoryCmd};
pub use paths::Paths;
pub use plugins::Plugins;
pub(crate) use registry::is_builtin_name;
pub use registry::{SlashCommand, SlashOutcome, SlashRegistry, render_help};
pub use runtime::{Diagnostics, Sessions, System, Tools};
pub use selection::{Model, Provider};

#[cfg(test)]
include!("tests.rs");
