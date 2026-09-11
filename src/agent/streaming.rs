use std::time::Instant;

use serde_json::Value;

use crate::agent::{ModelCaps, tool_parse};
use crate::error::Result;
use crate::ledger::as_ms;
use crate::providers::{ChatRequest, Provider, StreamEvent, StreamSink, Usage};

pub(crate) struct ProviderTurn {
    pub(crate) message: Value,
    pub(crate) tool_calls: Vec<Value>,
    pub(crate) usage: Option<Usage>,
    pub(crate) model_ms: u64,
    pub(crate) ttft_ms: Option<u64>,
}

pub(crate) async fn request<F>(
    provider: &dyn Provider,
    request: ChatRequest,
    caps: ModelCaps,
    sink: &mut F,
) -> Result<ProviderTurn>
where
    F: FnMut(StreamEvent<'_>) + Send,
{
    let model_started = Instant::now();
    let mut ttft = None;
    let response = {
        let mut timed = |event: StreamEvent<'_>| {
            if ttft.is_none() && matches!(event, StreamEvent::Content(_)) {
                ttft = Some(as_ms(model_started.elapsed()));
            }
            sink(event);
        };
        let sink: StreamSink<'_> = &mut timed;
        provider.chat_stream(request, sink).await?
    };
    let model_ms = as_ms(model_started.elapsed());

    let mut message = response.message;
    let mut tool_calls = message
        .get("tool_calls")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if tool_calls.is_empty() && !caps.supports_native_tool_calls {
        if let Some(content) = message.get("content").and_then(Value::as_str) {
            if let Some(parsed) = tool_parse::parse_text_tool_calls(content) {
                tool_calls = parsed.clone();
                message["tool_calls"] = Value::Array(parsed);
            }
        }
    }

    Ok(ProviderTurn {
        message,
        tool_calls,
        usage: response.usage,
        model_ms,
        ttft_ms: ttft,
    })
}
