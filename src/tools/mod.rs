//! Tool registry and the bundled built-in tools the agent ships
//! with.
//!
//! The [`Tool`] trait is intentionally narrow — `name`,
//! `description`, JSON-Schema `parameters`, and an async `call`
//! that returns a string. The model picks tools off
//! `Registry::openai_schemas`; the agent loop dispatches by
//! name through `Registry::call`.
//!
//! Submodules:
//! - [`bash`] — shell execution with timeout + process-group kill.
//! - [`context`] — `ToolContext`, the per-agent shared state
//!   (read-set, cwd, optional `ReadLogger`) tools see.
//! - [`edit`] — surgical string-replace edits, gated on a prior
//!   `Read` of the same file.
//! - [`glob`] — fast filename matching.
//! - [`grep`] — content search via ripgrep when available.
//! - [`notes`] — `WriteNote` / `SearchNotes` / `ListNotes` over a
//!   `NotesStore`.
//! - [`read`] — file reads with truncation + read-set tracking.
//! - [`subprocess`] — declared external CLI tools from config.
//! - [`task`] — `Task` subagent dispatch via a `SubagentSpawner`.
//! - `write` — clobber-safe whole-file writes.

use async_trait::async_trait;
use serde_json::{Value, json};
use std::collections::HashMap;

use crate::error::{Result, ToolError};

pub mod bash;
pub mod context;
pub mod edit;
pub mod glob;
pub mod grep;
pub mod notes;
pub mod read;
pub mod show_full;
pub mod subprocess;
pub mod task;
pub mod util;
pub mod write;

pub use context::ToolContext;

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> Value;
    async fn run(&self, args: Value, ctx: &ToolContext) -> Result<String>;
}

#[derive(Default)]
pub struct Registry {
    tools: HashMap<String, Box<dyn Tool>>,
    order: Vec<String>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<T: Tool + 'static>(&mut self, tool: T) {
        self.register_box(Box::new(tool));
    }

    /// Insert an already-boxed tool. The plugin loader uses this to
    /// register Lua-backed `Tool` impls without re-boxing.
    pub fn register_box(&mut self, tool: Box<dyn Tool>) {
        let name = tool.name().to_string();
        if !self.tools.contains_key(&name) {
            self.order.push(name.clone());
        }
        self.tools.insert(name, tool);
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(|b| b.as_ref())
    }

    /// Drop a registered tool by name. Used by `/plugins reload` so
    /// stale plugin tools come out of the registry before the
    /// freshly-loaded ones go back in. Returns `true` if a tool with
    /// that name was present.
    pub fn remove(&mut self, name: &str) -> bool {
        let had = self.tools.remove(name).is_some();
        if had {
            self.order.retain(|n| n != name);
        }
        had
    }

    /// Iterate over registered tools in registration order. Used by the
    /// `/tools` slash command and by future plugin hooks.
    pub fn iter(&self) -> impl Iterator<Item = &dyn Tool> {
        self.order
            .iter()
            .filter_map(|n| self.tools.get(n).map(|b| b.as_ref()))
    }

    pub async fn dispatch(&self, name: &str, args: Value, ctx: &ToolContext) -> Result<String> {
        let tool = self
            .get(name)
            .ok_or_else(|| ToolError::Unknown(name.to_string()))?;
        tool.run(args, ctx).await
    }

    /// OpenAI-compatible tool schemas: `[{type:"function", function:{name,description,parameters}}, ...]`
    pub fn openai_schemas(&self) -> Vec<Value> {
        self.order
            .iter()
            .filter_map(|n| self.tools.get(n))
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name(),
                        "description": t.description(),
                        "parameters": t.parameters(),
                    }
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;

    #[async_trait]
    impl Tool for Echo {
        fn name(&self) -> &str {
            "Echo"
        }
        fn description(&self) -> &str {
            "Returns the `text` argument"
        }
        fn parameters(&self) -> Value {
            json!({
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"]
            })
        }
        async fn run(&self, args: Value, _ctx: &ToolContext) -> Result<String> {
            Ok(args["text"].as_str().unwrap_or("").to_string())
        }
    }

    #[tokio::test]
    async fn dispatch_unknown_returns_unknown_tool_error() {
        let reg = Registry::new();
        let ctx = ToolContext::new();
        let err = reg.dispatch("Nope", json!({}), &ctx).await.unwrap_err();
        assert!(err.to_string().contains("unknown tool: Nope"));
    }

    #[tokio::test]
    async fn dispatch_runs_registered_tool() {
        let mut reg = Registry::new();
        reg.register(Echo);
        let ctx = ToolContext::new();
        let out = reg
            .dispatch("Echo", json!({ "text": "hello" }), &ctx)
            .await
            .unwrap();
        assert_eq!(out, "hello");
    }

    #[test]
    fn openai_schemas_preserve_registration_order() {
        let mut reg = Registry::new();
        reg.register(Echo);
        let schemas = reg.openai_schemas();
        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0]["type"], "function");
        assert_eq!(schemas[0]["function"]["name"], "Echo");
        assert!(schemas[0]["function"]["parameters"].is_object());
    }

    #[test]
    fn re_registering_same_name_replaces_but_keeps_position() {
        let mut reg = Registry::new();
        reg.register(Echo);
        reg.register(Echo);
        assert_eq!(reg.openai_schemas().len(), 1);
    }
}
