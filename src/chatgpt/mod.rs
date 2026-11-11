//! An aisdk provider for the ChatGPT backend (Codex), used with a ChatGPT
//! sign-in instead of an API key.
//!
//! aisdk's OpenAI provider cannot talk to this backend: it only streams, it
//! does not label its stream, and its request rules differ. So this module
//! implements aisdk's `LanguageModel` for it directly, and the rest of stonks
//! uses it through `LanguageModelRequest` like any other provider.

mod events;
mod request;
mod sse;

use std::collections::VecDeque;
use std::fmt;
use std::time::Duration;

use aisdk::core::capabilities::{
    ReasoningSupport, StructuredOutputSupport, TextInputSupport, TextOutputSupport, ToolCallSupport,
};
use aisdk::core::language_model::{
    LanguageModelOptions, LanguageModelResponse, LanguageModelResponseContentType,
    LanguageModelStreamChunk,
    LanguageModelStreamChunkType, ProviderStream,
};
use aisdk::core::{LanguageModel, Tool};
use aisdk::{Error, Result};
use async_trait::async_trait;
use futures::StreamExt;
use reqwest::StatusCode;
use serde_json::Value;

use crate::auth::flow;
use events::EventMapper;
use sse::SseDecoder;

pub const DEFAULT_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
const ORIGINATOR: &str = "pi";
const MAX_ATTEMPTS: u32 = 3;

#[derive(Clone)]
pub struct ChatGpt {
    model: String,
    access_token: String,
    account_id: String,
    base_url: String,
    tools: Vec<Tool>,
    http: reqwest::Client,
}

impl ChatGpt {
    /// `access_token` and `account_id` come from the ChatGPT sign-in.
    pub fn new(
        model: impl Into<String>,
        access_token: impl Into<String>,
        account_id: impl Into<String>,
    ) -> Self {
        Self {
            model: model.into(),
            access_token: access_token.into(),
            account_id: account_id.into(),
            base_url: DEFAULT_BASE_URL.to_owned(),
            tools: Vec::new(),
            http: reqwest::Client::new(),
        }
    }

    pub fn base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Declares the tools the model may call. aisdk does not expose a
    /// request's tool list to providers outside its own crate, so give the
    /// same tools here that are registered on the request with `with_tool`.
    pub fn tools(mut self, tools: Vec<Tool>) -> Self {
        self.tools = tools;
        self
    }

    /// Sends the request, retrying briefly on transient failures.
    async fn send(&self, options: &LanguageModelOptions) -> Result<reqwest::Response> {
        let body = request::body(&self.model, options, &self.tools);
        let url = format!("{}/responses", self.base_url.trim_end_matches('/'));
        let request_id = flow::random_token();
        let mut attempt = 1;
        loop {
            let mut request = self
                .http
                .post(&url)
                .bearer_auth(&self.access_token)
                .header("ChatGPT-Account-ID", &self.account_id)
                .header("originator", ORIGINATOR)
                .header("OpenAI-Beta", "responses=experimental")
                .header("Accept", "text/event-stream")
                .header("User-Agent", concat!("stonks/", env!("CARGO_PKG_VERSION")))
                .header("session_id", &request_id)
                .header("x-client-request-id", &request_id);
            // Workspaces pinned to a region refuse requests that do not name it.
            if let Some(residency) = residency(&self.access_token) {
                request = request.header("x-openai-internal-codex-residency", residency);
            }
            for (name, value) in options.headers.iter().flatten() {
                request = request.header(name, value);
            }
            let response = request.json(&body).send().await.map_err(|err| Error::ApiError {
                status_code: err.status(),
                details: format!("could not reach the ChatGPT backend: {err}"),
            })?;
            let status = response.status();
            if status.is_success() {
                return Ok(response);
            }
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok()?.parse::<u64>().ok());
            let failure = Failure::read(status, response).await;
            if attempt < MAX_ATTEMPTS && failure.transient {
                let wait = retry_after.unwrap_or(1 << (attempt - 1)).min(10);
                tokio::time::sleep(Duration::from_secs(wait)).await;
                attempt += 1;
                continue;
            }
            return Err(Error::ApiError { status_code: Some(status), details: failure.details });
        }
    }
}

impl fmt::Debug for ChatGpt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The access token is deliberately left out.
        f.debug_struct("ChatGpt")
            .field("model", &self.model)
            .field("base_url", &self.base_url)
            .field("tools", &self.tools)
            .finish_non_exhaustive()
    }
}

impl TextInputSupport for ChatGpt {}
impl TextOutputSupport for ChatGpt {}
impl ToolCallSupport for ChatGpt {}
impl StructuredOutputSupport for ChatGpt {}
impl ReasoningSupport for ChatGpt {}

#[async_trait]
impl LanguageModel for ChatGpt {
    fn name(&self) -> String {
        self.model.clone()
    }

    /// The backend only streams, so this collects the stream.
    async fn generate_text(
        &mut self,
        options: LanguageModelOptions,
    ) -> Result<LanguageModelResponse> {
        let mut stream = self.stream_text(options).await?;
        let mut response = LanguageModelResponse { contents: Vec::new(), usage: None };
        while let Some(chunks) = stream.next().await {
            for chunk in chunks? {
                match chunk {
                    LanguageModelStreamChunk::Done(message) => {
                        response.usage = message.usage.or(response.usage);
                        // As with aisdk's OpenAI provider, reasoning summaries
                        // are not part of a non-streaming result.
                        if !matches!(
                            message.content,
                            LanguageModelResponseContentType::Reasoning { .. }
                        ) {
                            response.contents.push(message.content);
                        }
                    }
                    LanguageModelStreamChunk::Delta(LanguageModelStreamChunkType::Failed(why)) => {
                        return Err(Error::ApiError { status_code: None, details: why });
                    }
                    LanguageModelStreamChunk::Delta(LanguageModelStreamChunkType::Incomplete(
                        why,
                    )) => {
                        return Err(Error::ApiError {
                            status_code: None,
                            details: format!("the response was cut short: {why}"),
                        });
                    }
                    LanguageModelStreamChunk::Delta(_) => {}
                }
            }
        }
        Ok(response)
    }

    async fn stream_text(&mut self, options: LanguageModelOptions) -> Result<ProviderStream> {
        let response = self.send(&options).await?;
        let state = StreamState {
            response,
            decoder: SseDecoder::default(),
            mapper: EventMapper::default(),
            ready: VecDeque::new(),
            ended: false,
        };
        Ok(Box::pin(futures::stream::unfold(state, |mut state| async move {
            state.next().await.map(|item| (item, state))
        })))
    }
}

struct StreamState {
    response: reqwest::Response,
    decoder: SseDecoder,
    mapper: EventMapper,
    ready: VecDeque<Result<Vec<LanguageModelStreamChunk>>>,
    ended: bool,
}

impl StreamState {
    async fn next(&mut self) -> Option<Result<Vec<LanguageModelStreamChunk>>> {
        loop {
            if let Some(item) = self.ready.pop_front() {
                return Some(item);
            }
            if self.ended {
                return None;
            }
            match self.response.chunk().await {
                Ok(Some(bytes)) => match self.decoder.push(&bytes) {
                    Ok(events) => events.iter().for_each(|data| self.event(data)),
                    Err(sse::FrameTooLarge) => self.fail("the ChatGPT backend sent an oversized event"),
                },
                Ok(None) => {
                    if let Some(data) = self.decoder.finish() {
                        self.event(&data);
                    }
                    if !self.mapper.finished {
                        self.fail("the ChatGPT backend closed the stream before the response finished");
                    }
                    self.ended = true;
                }
                Err(err) => self.fail(&format!("the ChatGPT backend stream broke: {err}")),
            }
        }
    }

    fn event(&mut self, data: &str) {
        if self.ended || data == "[DONE]" {
            return;
        }
        // An event that is not JSON carries nothing this provider understands.
        if let Ok(event) = serde_json::from_str::<Value>(data) {
            let chunks = self.mapper.map(&event);
            if !chunks.is_empty() {
                self.ready.push_back(Ok(chunks));
            }
            // Nothing after the closing event matters; stop reading.
            self.ended = self.mapper.finished;
        }
    }

    fn fail(&mut self, details: &str) {
        self.ready.push_back(Err(Error::ApiError { status_code: None, details: details.to_owned() }));
        self.ended = true;
    }
}

/// A non-2xx reply, reduced to a message and whether retrying could help.
struct Failure {
    details: String,
    transient: bool,
}

impl Failure {
    async fn read(status: StatusCode, response: reqwest::Response) -> Self {
        let text = response.text().await.unwrap_or_default();
        let body: Value = serde_json::from_str(&text).unwrap_or_default();
        let error = &body["error"];
        let code = error["code"].as_str().or(error["type"].as_str()).unwrap_or_default();
        let message = body["detail"]
            .as_str()
            .or(error["message"].as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| text.chars().take(500).collect());

        // A used-up plan allowance also arrives as 429, but waiting a few
        // seconds will not fix it.
        let out_of_quota = ["usage_limit_reached", "usage_not_included", "insufficient_quota"]
            .iter()
            .any(|marker| code.contains(marker));
        let details = if out_of_quota {
            format!("the ChatGPT plan's usage limit is reached: {message}")
        } else if status == StatusCode::UNAUTHORIZED {
            format!("the ChatGPT sign-in was rejected; run `stonks login --force` ({message})")
        } else {
            message
        };
        let transient = !out_of_quota
            && matches!(
                status,
                StatusCode::TOO_MANY_REQUESTS
                    | StatusCode::BAD_GATEWAY
                    | StatusCode::SERVICE_UNAVAILABLE
                    | StatusCode::GATEWAY_TIMEOUT
            );
        Self { details, transient }
    }
}

/// The data-residency region named in the sign-in token, if any.
fn residency(access_token: &str) -> Option<String> {
    let claims = flow::jwt_payload(access_token)?;
    let auth = &claims["https://api.openai.com/auth"];
    let region = auth["chatgpt_data_residency"].as_str().or(auth["chatgpt_compute_residency"].as_str())?;
    (!region.trim().is_empty()).then(|| region.trim().to_owned())
}

#[cfg(test)]
mod tests;
