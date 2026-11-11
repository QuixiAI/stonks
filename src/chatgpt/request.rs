//! Builds the request body for the ChatGPT backend's Responses endpoint.
//!
//! The backend differs from api.openai.com: the system prompt must be sent as
//! `instructions`, responses are never stored, and replies are always streamed.

use aisdk::core::language_model::{LanguageModelResponseContentType, ReasoningEffort};
use aisdk::core::language_model::LanguageModelOptions;
use aisdk::core::{Message, Tool};
use serde_json::{Map, Value, json};

const DEFAULT_INSTRUCTIONS: &str = "You are a helpful assistant.";

pub fn body(model: &str, options: &LanguageModelOptions, tools: &[Tool]) -> Value {
    let mut instructions: Vec<String> = options.system.iter().cloned().collect();
    let mut input = Vec::new();
    for message in options.messages() {
        match message {
            // The backend rejects system messages in `input`. aisdk passes the
            // system prompt both as `options.system` and as a message.
            Message::System(system) => {
                if !instructions.contains(&system.content) {
                    instructions.push(system.content);
                }
            }
            Message::Developer(text) => input.push(text_message("developer", &text)),
            Message::User(user) => input.push(text_message("user", &user.content)),
            Message::Assistant(assistant) => match assistant.content {
                LanguageModelResponseContentType::Text(text) => input.push(json!({
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": text, "annotations": [] }],
                })),
                LanguageModelResponseContentType::ToolCall(call) => input.push(json!({
                    "type": "function_call",
                    "call_id": call.tool.id,
                    "name": call.tool.name,
                    "arguments": call.input.to_string(),
                })),
                // Reasoning cannot be replayed: nothing is stored server-side and
                // aisdk keeps only the summary, not the encrypted content.
                LanguageModelResponseContentType::Reasoning { .. }
                | LanguageModelResponseContentType::NotSupported(_) => {}
            },
            Message::Tool(result) => input.push(json!({
                "type": "function_call_output",
                "call_id": result.tool.id,
                "output": match result.output {
                    Ok(Value::String(text)) => text,
                    Ok(other) => other.to_string(),
                    Err(err) => format!("Error: {err}"),
                },
            })),
        }
    }
    instructions.retain(|part| !part.trim().is_empty());

    let mut body = Map::new();
    body.insert("model".into(), json!(model));
    body.insert("store".into(), json!(false));
    body.insert("stream".into(), json!(true));
    body.insert(
        "instructions".into(),
        json!(if instructions.is_empty() {
            DEFAULT_INSTRUCTIONS.to_owned()
        } else {
            instructions.join("\n\n")
        }),
    );
    body.insert("input".into(), Value::Array(input));

    if let Some(schema) = &options.schema {
        let schema = serde_json::to_value(schema).unwrap_or_default();
        body.insert(
            "text".into(),
            json!({ "format": {
                "type": "json_schema",
                "name": format_name(schema["title"].as_str().unwrap_or("response")),
                "schema": schema,
                "strict": false,
            }}),
        );
    }
    if !tools.is_empty() {
        body.insert("tools".into(), tools.iter().map(tool).collect());
        body.insert("tool_choice".into(), json!("auto"));
        body.insert("parallel_tool_calls".into(), json!(true));
    }
    if let Some(effort) = options.reasoning_effort {
        let effort = match effort {
            ReasoningEffort::None => "none",
            ReasoningEffort::Low => "low",
            ReasoningEffort::Medium => "medium",
            ReasoningEffort::High => "high",
            ReasoningEffort::XHigh => "xhigh",
        };
        body.insert(
            "reasoning".into(),
            if effort == "none" {
                json!({ "effort": effort })
            } else {
                json!({ "effort": effort, "summary": "auto" })
            },
        );
    }
    // Sampling settings (temperature, top_p, max_output_tokens) are left out:
    // the backend rejects them for the models it serves.

    // Caller-supplied fields win, as with aisdk's own providers.
    if let Some(extra) = &options.body {
        body.extend(extra.clone());
    }
    Value::Object(body)
}

fn text_message(role: &str, text: &str) -> Value {
    json!({
        "type": "message",
        "role": role,
        "content": [{ "type": "input_text", "text": text }],
    })
}

fn tool(tool: &Tool) -> Value {
    let mut parameters = tool.input_schema.clone().to_value();
    if !parameters["properties"].is_object() {
        parameters["properties"] = json!({});
    }
    json!({
        "type": "function",
        "name": tool.name,
        "description": tool.description,
        "parameters": parameters,
        // Strict mode would turn every optional property into a required one.
        "strict": false,
    })
}

/// The API accepts only `[A-Za-z0-9_-]` in a format name, up to 64 characters.
fn format_name(title: &str) -> String {
    let name: String = title
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .take(64)
        .collect();
    if name.is_empty() { "response".to_owned() } else { name }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_names_are_sanitized() {
        assert_eq!(format_name("Trade Review (v2)"), "Trade_Review__v2_");
        assert_eq!(format_name(""), "response");
        assert_eq!(format_name(&"x".repeat(100)).len(), 64);
    }
}
