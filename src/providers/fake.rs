//! In-memory scripted provider for tests.

use async_trait::async_trait;
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::Mutex;

use crate::error::{AgentError, Result};
use crate::providers::{ChatRequest, ChatResponse, Provider, StreamEvent, StreamSink, Usage};

pub struct FakeProvider {
    responses: Mutex<VecDeque<(Value, Option<Usage>)>>,
    /// Served once `responses` drains. `None` makes exhaustion an error,
    /// which is what a test asserting an exact call count wants.
    constant: Option<(Value, Option<Usage>)>,
    seen: Mutex<Vec<ChatRequest>>,
}

impl FakeProvider {
    pub fn new(responses: Vec<Value>) -> Self {
        Self::with_usage(responses.into_iter().map(|m| (m, None)).collect())
    }

    /// Scripted replies that also carry per-call token accounting. A
    /// `None` usage models a provider that reported nothing for that
    /// call — distinct from reporting zero.
    pub fn with_usage(responses: Vec<(Value, Option<Usage>)>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            constant: None,
            seen: Mutex::new(Vec::new()),
        }
    }

    /// Answer every call with the same message and usage. For tests that
    /// care what the agent does with a reply, not how many it asks for.
    pub fn constant(message: Value, usage: Option<Usage>) -> Self {
        Self {
            responses: Mutex::new(VecDeque::new()),
            constant: Some((message, usage)),
            seen: Mutex::new(Vec::new()),
        }
    }

    pub fn requests(&self) -> Vec<ChatRequest> {
        self.seen.lock().unwrap().clone()
    }
}

/// Provider that fails the test if the code under test calls it. Asserts
/// a negative — "this path must not reach the model" — that a scripted
/// reply can only approximate.
pub struct NeverCalled(pub &'static str);

#[async_trait]
impl Provider for NeverCalled {
    async fn chat(&self, _: ChatRequest) -> Result<ChatResponse> {
        unreachable!("{}", self.0)
    }
}

#[async_trait]
impl Provider for FakeProvider {
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse> {
        self.seen.lock().unwrap().push(req);
        let (message, usage) = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .or_else(|| self.constant.clone())
            .ok_or_else(|| AgentError::Provider("FakeProvider exhausted".into()))?;
        Ok(ChatResponse { message, usage })
    }

    /// Splits scripted content into two halves at a char boundary so streaming
    /// tests can assert multi-chunk delivery without depending on real-network
    /// chunk timing.
    async fn chat_stream(&self, req: ChatRequest, sink: StreamSink<'_>) -> Result<ChatResponse> {
        let resp = self.chat(req).await?;
        if let Some(s) = resp.message.get("content").and_then(|v| v.as_str()) {
            if s.len() >= 2 {
                let mut cut = s.len() / 2;
                while cut > 0 && !s.is_char_boundary(cut) {
                    cut -= 1;
                }
                if cut == 0 || cut == s.len() {
                    sink(StreamEvent::Content(s));
                } else {
                    sink(StreamEvent::Content(&s[..cut]));
                    sink(StreamEvent::Content(&s[cut..]));
                }
            } else if !s.is_empty() {
                sink(StreamEvent::Content(s));
            }
        }
        Ok(resp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn replays_scripted_responses_in_order() {
        let p = FakeProvider::new(vec![
            json!({"role": "assistant", "content": "first"}),
            json!({"role": "assistant", "content": "second"}),
        ]);

        let r1 = p
            .chat(ChatRequest {
                model: "x".into(),
                messages: vec![],
                tools: vec![],
            })
            .await
            .unwrap();
        assert_eq!(r1.message["content"], "first");

        let r2 = p
            .chat(ChatRequest {
                model: "x".into(),
                messages: vec![],
                tools: vec![],
            })
            .await
            .unwrap();
        assert_eq!(r2.message["content"], "second");
    }

    #[tokio::test]
    async fn exhausting_responses_yields_provider_error() {
        let p = FakeProvider::new(vec![]);
        let err = p
            .chat(ChatRequest {
                model: "x".into(),
                messages: vec![],
                tools: vec![],
            })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("exhausted"));
    }

    #[tokio::test]
    async fn records_seen_requests() {
        let p = FakeProvider::new(vec![json!({"role": "assistant", "content": "ok"})]);
        let req = ChatRequest {
            model: "m".into(),
            messages: vec![json!({"role": "user", "content": "hi"})],
            tools: vec![],
        };
        p.chat(req).await.unwrap();
        assert_eq!(p.requests().len(), 1);
        assert_eq!(p.requests()[0].model, "m");
    }
}
