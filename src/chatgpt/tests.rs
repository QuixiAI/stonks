//! End-to-end tests: real aisdk requests against a local stand-in for the
//! backend, which (like the real one) streams without a `Content-Type`.

use std::sync::{Arc, Mutex};

use aisdk::core::utils::step_count_is;
use aisdk::core::{LanguageModelRequest, LanguageModelStreamChunkType, Tool};
use aisdk::macros::tool;
use futures::StreamExt;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::ChatGpt;

struct Request {
    head: String,
    body: Value,
}

/// Serves `replies` in order, one per request, and records what it was sent.
struct Backend {
    url: String,
    requests: Arc<Mutex<Vec<Request>>>,
}

impl Backend {
    async fn start(replies: Vec<(u16, String)>) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let url = format!("http://{}/backend-api/codex", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        tokio::spawn(async move {
            for (status, reply) in replies {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = Vec::new();
                let (head, body) = loop {
                    let mut chunk = [0u8; 4096];
                    let n = socket.read(&mut chunk).await.unwrap();
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf).into_owned();
                    let Some((head, body)) = text.split_once("\r\n\r\n") else { continue };
                    let length: usize = head
                        .lines()
                        .filter_map(|line| line.split_once(':'))
                        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .map(|(_, value)| value.trim().parse().unwrap())
                        .unwrap_or(0);
                    if body.len() >= length {
                        break (head.to_owned(), body.to_owned());
                    }
                };
                recorded
                    .lock()
                    .unwrap()
                    .push(Request { head, body: serde_json::from_str(&body).unwrap() });
                // No Content-Type and no Content-Length, as the real backend does.
                let reply = format!("HTTP/1.1 {status} X\r\nConnection: close\r\n\r\n{reply}");
                socket.write_all(reply.as_bytes()).await.unwrap();
                socket.shutdown().await.unwrap();
            }
        });
        Self { url, requests }
    }

    fn model(&self) -> ChatGpt {
        ChatGpt::new("gpt-test", "token-123", "acct-9").base_url(&self.url)
    }

    fn requests(&self) -> std::sync::MutexGuard<'_, Vec<Request>> {
        self.requests.lock().unwrap()
    }
}

fn sse(events: &[Value]) -> String {
    events.iter().map(|event| format!("event: {}\ndata: {event}\n\n", event["type"].as_str().unwrap())).collect()
}

fn text_reply(text: &str) -> String {
    let item = json!({ "type": "message", "role": "assistant",
        "content": [{ "type": "output_text", "text": text }] });
    sse(&[
        json!({ "type": "response.created", "response": {} }),
        json!({ "type": "response.output_item.added", "item": { "type": "message" } }),
        json!({ "type": "response.output_text.delta", "delta": text }),
        json!({ "type": "response.output_text.done", "text": text }),
        json!({ "type": "response.output_item.done", "item": item }),
        json!({ "type": "response.completed", "response": {
            "output": [item],
            "usage": { "input_tokens": 11, "output_tokens": 7,
                "input_tokens_details": { "cached_tokens": 2 },
                "output_tokens_details": { "reasoning_tokens": 1 } },
        }}),
    ])
}

#[tool]
/// Look up the last price of a stock.
fn last_price(symbol: String) -> Tool {
    Ok(format!("{symbol} 101.50"))
}

#[tokio::test]
async fn generates_text_and_sends_the_backend_request_shape() {
    let backend = Backend::start(vec![(200, text_reply("ok"))]).await;
    let response = LanguageModelRequest::builder()
        .model(backend.model())
        .system("Be terse.")
        .prompt("Say ok.")
        .build()
        .generate_text()
        .await
        .unwrap();

    assert_eq!(response.text().as_deref(), Some("ok"));
    let usage = response.usage();
    assert_eq!((usage.input_tokens, usage.output_tokens), (Some(11), Some(7)));

    let requests = backend.requests();
    let head = requests[0].head.to_lowercase();
    assert!(head.starts_with("post /backend-api/codex/responses "));
    assert!(head.contains("authorization: bearer token-123"));
    assert!(head.contains("chatgpt-account-id: acct-9"));
    let body = &requests[0].body;
    assert_eq!(body["model"], "gpt-test");
    assert_eq!(body["instructions"], "Be terse.");
    assert_eq!(body["store"], false);
    assert_eq!(body["stream"], true);
    assert_eq!(
        body["input"],
        json!([{ "type": "message", "role": "user",
            "content": [{ "type": "input_text", "text": "Say ok." }] }])
    );
    assert!(body.get("tools").is_none());
}

#[tokio::test]
async fn streams_text_deltas() {
    let backend = Backend::start(vec![(200, text_reply("hello"))]).await;
    let mut response = LanguageModelRequest::builder()
        .model(backend.model())
        .prompt("Hi")
        .build()
        .stream_text()
        .await
        .unwrap();
    let mut text = String::new();
    while let Some(chunk) = response.stream.next().await {
        if let LanguageModelStreamChunkType::TextDelta(delta) = chunk {
            text.push_str(&delta);
        }
    }
    assert_eq!(text, "hello");
    // With no system prompt the backend still requires instructions.
    assert_eq!(backend.requests()[0].body["instructions"], "You are a helpful assistant.");
}

#[tokio::test]
async fn runs_a_tool_call_round_trip() {
    let call = json!({ "type": "function_call", "id": "fc_1", "call_id": "call_1",
        "name": "last_price", "arguments": "{\"symbol\":\"TGT\"}" });
    let first = sse(&[
        json!({ "type": "response.output_item.added", "item": call }),
        json!({ "type": "response.output_item.done", "item": call }),
        // The closing event may omit `output`; the items seen above are used.
        json!({ "type": "response.completed", "response": { "output": [] } }),
    ]);
    let backend = Backend::start(vec![(200, first), (200, text_reply("TGT is at 101.50"))]).await;

    let response = LanguageModelRequest::builder()
        .model(backend.model().tools(vec![last_price()]))
        .prompt("Price of TGT?")
        .with_tool(last_price())
        .stop_when(step_count_is(3))
        .build()
        .generate_text()
        .await
        .unwrap();
    assert_eq!(response.text().as_deref(), Some("TGT is at 101.50"));

    let requests = backend.requests();
    assert_eq!(requests.len(), 2);
    let tools = &requests[0].body["tools"];
    assert_eq!(tools[0]["type"], "function");
    assert_eq!(tools[0]["name"], "last_price");
    assert_eq!(tools[0]["strict"], false);
    assert!(tools[0]["parameters"]["properties"]["symbol"].is_object());

    // The second request replays the call and carries the tool's result.
    let input = requests[1].body["input"].as_array().unwrap();
    let call = input.iter().find(|item| item["type"] == "function_call").unwrap();
    assert_eq!((call["call_id"].as_str(), call["name"].as_str()), (Some("call_1"), Some("last_price")));
    let output = input.iter().find(|item| item["type"] == "function_call_output").unwrap();
    assert_eq!(output["call_id"], "call_1");
    assert_eq!(output["output"], "TGT 101.50");
}

#[tokio::test]
async fn reports_backend_errors_without_retrying_a_used_up_plan() {
    let limit = json!({ "error": { "type": "usage_limit_reached", "message": "Try again in 3 days." } });
    let backend = Backend::start(vec![(429, limit.to_string())]).await;
    let error = LanguageModelRequest::builder()
        .model(backend.model())
        .prompt("Hi")
        .build()
        .generate_text()
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("usage limit"), "{error}");
    assert!(error.contains("Try again in 3 days."), "{error}");
    assert_eq!(backend.requests().len(), 1);
}

#[tokio::test]
async fn retries_a_transient_failure() {
    let backend = Backend::start(vec![(503, "busy".to_owned()), (200, text_reply("ok"))]).await;
    let response = LanguageModelRequest::builder()
        .model(backend.model())
        .prompt("Hi")
        .build()
        .generate_text()
        .await
        .unwrap();
    assert_eq!(response.text().as_deref(), Some("ok"));
    assert_eq!(backend.requests().len(), 2);
}

#[tokio::test]
async fn a_stream_cut_short_is_an_error() {
    let partial = sse(&[json!({ "type": "response.output_text.delta", "delta": "par" })]);
    let backend = Backend::start(vec![(200, partial)]).await;
    let error = LanguageModelRequest::builder()
        .model(backend.model())
        .prompt("Hi")
        .build()
        .generate_text()
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("before the response finished"), "{error}");
}

#[tokio::test]
async fn a_failed_response_is_an_error() {
    let failed = sse(&[json!({ "type": "response.failed", "response": {
        "error": { "code": "server_error", "message": "boom" } } })]);
    let backend = Backend::start(vec![(200, failed)]).await;
    let error = LanguageModelRequest::builder()
        .model(backend.model())
        .prompt("Hi")
        .build()
        .generate_text()
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("server_error: boom"), "{error}");
}
