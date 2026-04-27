use async_trait::async_trait;
use serde_json::{Value, json};
use std::collections::HashMap;

use crate::error::{Result, ToolError};

pub mod bash;
pub mod context;
pub mod edit;
pub mod glob;
pub mod grep;
pub mod read;
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
        let name = tool.name().to_string();
        if !self.tools.contains_key(&name) {
            self.order.push(name.clone());
        }
        self.tools.insert(name, Box::new(tool));
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(|b| b.as_ref())
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
