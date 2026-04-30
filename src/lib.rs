//! `oli` — minimal, hackable, single-binary terminal coding agent.
//!
//! This crate exposes both the binary (`oli`) and a library that
//! embedders can use to build their own coding agents on top of
//! the same primitives. The binary entry point lives in
//! `src/bin/oli.rs` and is a thin shim around the modules below.
//!
//! # Architectural overview
//!
//! The harness is organized around a small set of traits, each
//! with at least one bundled implementation. New providers, tools,
//! policies, etc. drop in by implementing the relevant trait — no
//! changes to the agent loop required.
//!
//! - [`Provider`] — chat-completion API (Anthropic, OpenAI-compat).
//! - [`Tool`] — a callable capability the model can invoke.
//! - [`Memory`] — conversation state strategy (linear+compact, RAG, etc.).
//! - [`Policy`] / [`Approver`] — gate before tool execution; ask the user
//!   when in doubt.
//! - [`Hook`] — observability hook fired around tool calls.
//! - [`SlashCommand`] — REPL-side commands (`/help`, `/cost`, …).
//! - [`SubagentSpawner`] — builds a fresh child agent for the `Task` tool.
//! - [`McpHandle`] — connection to an external MCP server providing tools.
//!
//! See `specs/README.md` (in the repo) for the full design rationale.
//!
//! [`Provider`]: providers::Provider
//! [`Tool`]: tools::Tool
//! [`Memory`]: agent::memory::Memory
//! [`Policy`]: policy::Policy
//! [`Approver`]: policy::Approver
//! [`Hook`]: hooks::Hook
//! [`SlashCommand`]: repl::slash::SlashCommand
//! [`SubagentSpawner`]: tools::task::SubagentSpawner
//! [`McpHandle`]: mcp::McpHandle

pub mod agent;
pub mod bootstrap;
pub mod config;
pub mod diagnostics;
pub mod error;
pub mod hooks;
pub mod mcp;
pub mod notes;
pub mod plugins;
pub mod policy;
pub mod providers;
pub mod repl;
pub mod tools;
pub mod tui;

// ----- Public re-exports (the most-used types for embedders) -----

pub use agent::Agent;
pub use agent::memory::{
    EmbeddingRagMemory, LinearWithCompact, Memory, OllamaEmbedder, PersistedMemory,
};
pub use config::Config;
pub use error::{AgentError, Result};
pub use hooks::Hook;
pub use mcp::McpHandle;
pub use policy::{Approver, Policy};
pub use providers::Provider;
pub use repl::slash::SlashCommand;
pub use tools::Tool;
pub use tools::task::SubagentSpawner;
