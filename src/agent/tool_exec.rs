use serde_json::Value;

use crate::hooks::{HookRegistry, PreToolDecision};
use crate::policy::{Decision, Policy};
use crate::tools::{Registry, ToolContext};

pub(crate) async fn execute(
    tools: &Registry,
    ctx: &ToolContext,
    policy: &dyn Policy,
    hooks: &HookRegistry,
    name: &str,
    args: Value,
) -> String {
    let decision = hooks.dispatch_pre_tool_use(name, args).await;
    let (final_args, raw_result) = match decision {
        PreToolDecision::Continue { args } => {
            let result = match policy.check(name, &args) {
                Decision::Allow => match tools.dispatch(name, args.clone(), ctx).await {
                    Ok(result) => result,
                    Err(error) => format!("Error: {error}"),
                },
                Decision::Deny(reason) => format!("policy denied {name}: {reason}"),
            };
            (args, result)
        }
        PreToolDecision::Skip { args, result } => (args, result),
    };

    hooks
        .dispatch_post_tool_use(name, &final_args, raw_result)
        .await
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use serde_json::json;

    use super::*;
    use crate::error::Result;
    use crate::policy::{AllowAll, DenyAll};
    use crate::tools::Tool;

    struct Echo;
    struct MustNotRun;

    #[async_trait]
    impl Tool for Echo {
        fn name(&self) -> &str {
            "Echo"
        }

        fn description(&self) -> &str {
            "echo input"
        }

        fn parameters(&self) -> Value {
            json!({"type": "object"})
        }

        async fn run(&self, args: Value, _: &ToolContext) -> Result<String> {
            Ok(args.to_string())
        }
    }

    #[async_trait]
    impl Tool for MustNotRun {
        fn name(&self) -> &str {
            "MustNotRun"
        }

        fn description(&self) -> &str {
            "fails if dispatched"
        }

        fn parameters(&self) -> Value {
            json!({"type": "object"})
        }

        async fn run(&self, _: Value, _: &ToolContext) -> Result<String> {
            panic!("denied tool was dispatched")
        }
    }

    fn registry() -> Registry {
        let mut tools = Registry::new();
        tools.register(Echo);
        tools
    }

    #[tokio::test]
    async fn allowed_calls_dispatch_without_driving_the_agent_loop() {
        let result = execute(
            &registry(),
            &ToolContext::new(),
            &AllowAll,
            &HookRegistry::new(),
            "Echo",
            json!({"value": 1}),
        )
        .await;

        assert_eq!(result, r#"{"value":1}"#);
    }

    #[tokio::test]
    async fn denied_calls_return_an_observation_without_dispatching() {
        let mut tools = Registry::new();
        tools.register(MustNotRun);
        let result = execute(
            &tools,
            &ToolContext::new(),
            &DenyAll,
            &HookRegistry::new(),
            "MustNotRun",
            json!({"value": 1}),
        )
        .await;

        assert!(result.contains("policy denied MustNotRun"));
    }
}
