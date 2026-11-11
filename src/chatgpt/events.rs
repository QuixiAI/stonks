//! Turns the backend's Responses stream events into aisdk stream chunks.

use aisdk::core::language_model::{
    LanguageModelResponseContentType, LanguageModelStreamChunk, LanguageModelStreamChunkType, Usage,
};
use aisdk::core::tools::ToolDetails;
use aisdk::core::{AssistantMessage, ToolCallInfo};
use aisdk::extensions::Extensions;
use serde_json::Value;

type Chunk = LanguageModelStreamChunk;
type Delta = LanguageModelStreamChunkType;

#[derive(Debug, Default)]
pub struct EventMapper {
    /// Finished output items, kept in case the closing event omits them.
    items: Vec<Value>,
    /// A closing event (completed, incomplete, failed, error) has been seen.
    pub finished: bool,
}

impl EventMapper {
    /// Maps one decoded event. Unknown event types yield nothing.
    pub fn map(&mut self, event: &Value) -> Vec<Chunk> {
        let delta = |kind: Delta| vec![Chunk::Delta(kind)];
        let text = |key: &str| event[key].as_str().unwrap_or_default().to_owned();
        match event["type"].as_str().unwrap_or_default() {
            "response.output_item.added" => match event["item"]["type"].as_str() {
                Some("message") => delta(Delta::TextStart),
                Some("reasoning") => delta(Delta::ReasoningStart),
                Some("function_call") => delta(Delta::ToolCallStart(ToolDetails {
                    name: event["item"]["name"].as_str().unwrap_or_default().to_owned(),
                    id: event["item"]["id"].as_str().unwrap_or_default().to_owned(),
                })),
                _ => Vec::new(),
            },
            "response.output_item.done" => {
                self.items.push(event["item"].clone());
                Vec::new()
            }
            "response.output_text.delta" => delta(Delta::TextDelta(text("delta"))),
            "response.output_text.done" => delta(Delta::TextEnd),
            "response.reasoning_summary_text.delta" => delta(Delta::ReasoningDelta(text("delta"))),
            "response.reasoning_summary_text.done" => delta(Delta::ReasoningEnd),
            "response.function_call_arguments.delta" => {
                delta(Delta::ToolCallDelta { id: text("item_id"), delta: text("delta") })
            }
            "response.completed" | "response.done" => {
                self.finished = true;
                self.completed(&event["response"])
            }
            "response.incomplete" => {
                self.finished = true;
                let reason = event["response"]["incomplete_details"]["reason"].as_str();
                delta(Delta::Incomplete(reason.unwrap_or("unknown").to_owned()))
            }
            "response.failed" => {
                self.finished = true;
                delta(Delta::Failed(error_text(&event["response"]["error"])))
            }
            "error" => {
                self.finished = true;
                let error = if event["error"].is_object() { &event["error"] } else { event };
                delta(Delta::Failed(error_text(error)))
            }
            _ => Vec::new(),
        }
    }

    /// One `Done` chunk per output item of the finished response.
    fn completed(&mut self, response: &Value) -> Vec<Chunk> {
        let usage = usage(&response["usage"]);
        let listed = response["output"].as_array().filter(|items| !items.is_empty());
        let items = listed.cloned().unwrap_or_else(|| std::mem::take(&mut self.items));
        items
            .iter()
            .filter_map(content)
            .map(|content| Chunk::Done(AssistantMessage { content, usage: Some(usage.clone()) }))
            .collect()
    }
}

fn content(item: &Value) -> Option<LanguageModelResponseContentType> {
    let joined = |parts: &Value, kind: &str| -> String {
        parts
            .as_array()
            .into_iter()
            .flatten()
            .filter(|part| part["type"] == kind)
            .filter_map(|part| part["text"].as_str())
            .collect()
    };
    match item["type"].as_str()? {
        "message" => {
            Some(LanguageModelResponseContentType::Text(joined(&item["content"], "output_text")))
        }
        "function_call" => {
            let mut call = ToolCallInfo::new(item["name"].as_str()?);
            call.id(item["call_id"].as_str()?);
            let arguments = item["arguments"].as_str().unwrap_or("{}");
            call.input(serde_json::from_str(arguments).unwrap_or_default());
            Some(LanguageModelResponseContentType::ToolCall(call))
        }
        "reasoning" => {
            let summary = joined(&item["summary"], "summary_text");
            (!summary.is_empty()).then(|| LanguageModelResponseContentType::Reasoning {
                content: summary,
                extensions: Extensions::default(),
            })
        }
        _ => None,
    }
}

fn usage(usage: &Value) -> Usage {
    let count = |value: &Value| value.as_u64().map(|n| n as usize);
    Usage {
        input_tokens: count(&usage["input_tokens"]),
        output_tokens: count(&usage["output_tokens"]),
        cached_tokens: count(&usage["input_tokens_details"]["cached_tokens"]),
        reasoning_tokens: count(&usage["output_tokens_details"]["reasoning_tokens"]),
    }
}

fn error_text(error: &Value) -> String {
    let message = error["message"].as_str().unwrap_or("the model call failed");
    match error["code"].as_str().or(error["type"].as_str()) {
        Some(code) => format!("{code}: {message}"),
        None => message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn done_contents(chunks: Vec<Chunk>) -> Vec<LanguageModelResponseContentType> {
        chunks
            .into_iter()
            .filter_map(|chunk| match chunk {
                Chunk::Done(message) => Some(message.content),
                Chunk::Delta(_) => None,
            })
            .collect()
    }

    #[test]
    fn completed_falls_back_to_items_seen_earlier() {
        let mut mapper = EventMapper::default();
        mapper.map(&json!({ "type": "response.output_item.done", "item": {
            "type": "message", "content": [
                { "type": "output_text", "text": "Hello, " },
                { "type": "output_text", "text": "world" },
            ]
        }}));
        let chunks = mapper.map(&json!({ "type": "response.completed", "response": {
            "output": [], "usage": { "input_tokens": 3, "output_tokens": 2 }
        }}));
        assert!(mapper.finished);
        match &done_contents(chunks)[..] {
            [LanguageModelResponseContentType::Text(text)] => assert_eq!(text, "Hello, world"),
            other => panic!("unexpected contents: {other:?}"),
        }
    }

    #[test]
    fn function_calls_become_tool_calls() {
        let mut mapper = EventMapper::default();
        let chunks = mapper.map(&json!({ "type": "response.completed", "response": { "output": [{
            "type": "function_call", "call_id": "call_1", "name": "quote",
            "arguments": "{\"symbol\":\"TGT\"}"
        }]}}));
        match &done_contents(chunks)[..] {
            [LanguageModelResponseContentType::ToolCall(call)] => {
                assert_eq!(call.tool.name, "quote");
                assert_eq!(call.tool.id, "call_1");
                assert_eq!(call.input, json!({ "symbol": "TGT" }));
            }
            other => panic!("unexpected contents: {other:?}"),
        }
    }

    #[test]
    fn failures_are_reported_with_their_code() {
        let mut mapper = EventMapper::default();
        let chunks = mapper.map(&json!({ "type": "response.failed", "response": {
            "error": { "code": "server_error", "message": "boom" }
        }}));
        assert!(mapper.finished);
        assert!(matches!(&chunks[..], [Chunk::Delta(Delta::Failed(text))] if text == "server_error: boom"));
    }
}
