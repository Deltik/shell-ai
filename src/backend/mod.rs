//! Backend abstraction layer for AI completion providers.
//!
//! This module provides a trait-based abstraction over different AI backends:
//! - OpenAI-compatible HTTP APIs (OpenAI, Groq, Azure, Ollama, Mistral)
//! - Native Anthropic Messages API
//! - Claude Code CLI (subprocess-based)

mod anthropic;
mod claude_code;
pub mod correction;
mod openai;

pub use anthropic::AnthropicBackend;
pub use claude_code::ClaudeCodeBackend;
pub use openai::OpenAiBackend;

use anyhow::Result;
use crate::http::{HttpError, SseStream};
use serde_json::json;
use std::thread;
use std::time::Duration;

/// Maximum number of retry attempts for rate limiting (HTTP 429).
const MAX_RETRIES: u32 = 5;

/// Base delay for exponential backoff in milliseconds.
const BASE_DELAY_MS: u64 = 1000;

/// Role for a message in conversation history.
#[derive(Debug, Clone)]
pub enum MessageRole {
    User,
    Assistant,
}

impl MessageRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
        }
    }
}

/// A prior message in a multi-turn conversation.
#[derive(Debug, Clone)]
pub struct HistoryMessage {
    pub role: MessageRole,
    pub content: String,
}

/// Build the JSON messages array from history + final user message.
/// Used by HTTP-based backends (OpenAI, Anthropic).
fn build_history_messages(request: &CompletionRequest) -> Vec<serde_json::Value> {
    let mut messages: Vec<serde_json::Value> = Vec::new();
    for msg in &request.message_history {
        messages.push(json!({"role": msg.role.as_str(), "content": &msg.content}));
    }
    messages.push(json!({"role": "user", "content": &request.user_message}));
    messages
}

/// Outcome of HTTP status handling.
enum HttpStatus {
    /// Stream is ready for SSE processing.
    Ready,
    /// Rate limited; already slept and notified callback. Caller should retry.
    Retry,
}

/// Attempt to connect with retryable network error handling.
///
/// Returns `Ok(Some(stream))` on success, `Ok(None)` if the caller should `continue`
/// the retry loop (network error with backoff already applied), or `Err` on final failure.
fn post_json_streaming_retryable(
    url: &str,
    bearer_token: Option<&str>,
    extra_headers: &[(&str, &str)],
    body: &serde_json::Value,
    attempt: u32,
    callback: &StreamCallback,
) -> Result<Option<SseStream>, BackendError> {
    match crate::http::post_json_streaming(url, bearer_token, extra_headers, body) {
        Ok(stream) => Ok(Some(stream)),
        Err(HttpError::Config(msg)) => {
            // Configuration errors are permanent — don't retry
            Err(BackendError::ConfigError(msg))
        }
        Err(HttpError::Network(msg)) => {
            if attempt < MAX_RETRIES {
                let delay_ms = BASE_DELAY_MS * (1 << attempt);
                log::debug!("Network error (attempt {}): {}", attempt + 1, msg);
                callback(StreamEvent::Backoff { attempt: attempt + 1, delay_ms });
                thread::sleep(Duration::from_millis(delay_ms));
                callback(StreamEvent::Retrying { attempt: attempt + 1 });
                Ok(None)
            } else {
                Err(BackendError::NetworkError(msg))
            }
        }
    }
}

/// Handle common HTTP error statuses (429 retry, 413, generic errors).
///
/// Returns `Ok(HttpStatus::Ready)` if the stream should be processed,
/// `Ok(HttpStatus::Retry)` if the caller should retry the request (stream is consumed),
/// or `Err(BackendError)` for terminal errors.
fn handle_http_status(
    stream: SseStream,
    attempt: u32,
    callback: &StreamCallback,
) -> Result<(HttpStatus, SseStream), BackendError> {
    let status = stream.status();

    if status == 429 {
        if attempt < MAX_RETRIES {
            let delay_ms = stream
                .retry_after_secs()
                .map(|secs| secs * 1000)
                .unwrap_or_else(|| BASE_DELAY_MS * (1 << attempt));
            callback(StreamEvent::Backoff { attempt: attempt + 1, delay_ms });
            thread::sleep(Duration::from_millis(delay_ms));
            callback(StreamEvent::Retrying { attempt: attempt + 1 });
            return Ok((HttpStatus::Retry, stream));
        }
        let body = stream.read_body().unwrap_or_default().trim().to_string();
        return Err(BackendError::RateLimited(
            if body.is_empty() { "Too many requests".to_string() } else { body }
        ));
    }

    if status == 413 {
        let body = stream.read_body().unwrap_or_default().trim().to_string();
        return Err(BackendError::RequestTooLarge(
            if body.is_empty() { "context length exceeded".to_string() } else { body }
        ));
    }

    if !(200..300).contains(&status) {
        let body = stream.read_body().unwrap_or_default().trim().to_string();
        return Err(BackendError::ApiError(format!(
            "HTTP {}: {}",
            status,
            if body.is_empty() { "Unknown error".to_string() } else { body }
        )));
    }

    Ok((HttpStatus::Ready, stream))
}

/// Append a text chunk to the accumulator and emit it through the callback.
/// Returns the callback's `StreamAction` (`Abort` to stop the stream early).
/// No-ops for empty text.
fn emit_text_delta(text: &str, accumulator: &mut String, callback: &StreamCallback) -> StreamAction {
    if text.is_empty() {
        return StreamAction::Continue;
    }
    accumulator.push_str(text);
    callback(StreamEvent::TextDelta(text.to_string()))
}

/// A completion request to be sent to an AI backend.
#[derive(Debug, Clone)]
pub struct CompletionRequest {
    /// System messages (instructions, documentation, etc.)
    /// For OpenAI-compatible: sent as a single merged system role message
    /// For Anthropic: concatenated into the system field
    /// For Claude Code: combined into the prompt
    pub system_messages: Vec<String>,

    /// Prior conversation turns (for correction retries).
    /// Inserted between system messages and `user_message`.
    pub message_history: Vec<HistoryMessage>,

    /// The user's message/prompt (always the final user turn)
    pub user_message: String,

    /// Optional JSON schema for structured output
    pub json_schema: Option<serde_json::Value>,

    /// Name for the JSON schema (used in API calls)
    pub schema_name: String,
}

/// A completion response from an AI backend.
#[derive(Debug, Clone)]
pub struct CompletionResponse {
    /// The content of the response (typically JSON when schema is provided)
    pub content: String,

    /// Whether the response was truncated due to max_tokens limit
    pub is_truncated: bool,
}

/// Error types specific to backend operations.
#[derive(Debug, Clone)]
pub enum BackendError {
    /// Request was too large (HTTP 413 or equivalent)
    RequestTooLarge(String),

    /// Rate limited (HTTP 429) - can be retried after backoff
    RateLimited(String),

    /// API returned an error
    ApiError(String),

    /// Network or connection error
    NetworkError(String),

    /// Configuration error (e.g., bad curl_cmd path)
    ConfigError(String),

    /// Failed to parse the response
    ParseError(String),

    /// Other errors
    Other(String),
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackendError::RequestTooLarge(msg) => write!(f, "Request too large: {}", msg),
            BackendError::RateLimited(msg) => write!(f, "Rate limited: {}", msg),
            BackendError::ApiError(msg) => write!(f, "API error: {}", msg),
            BackendError::NetworkError(msg) => write!(f, "Network error: {}", msg),
            BackendError::ConfigError(msg) => write!(f, "Configuration error: {}", msg),
            BackendError::ParseError(msg) => write!(f, "Parse error: {}", msg),
            BackendError::Other(e) => write!(f, "{}", e),
        }
    }
}

impl std::error::Error for BackendError {}

impl From<anyhow::Error> for BackendError {
    fn from(e: anyhow::Error) -> Self {
        BackendError::Other(e.to_string())
    }
}

/// Event emitted during streaming completion.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// A chunk of text (raw token) - the main response content.
    TextDelta(String),
    /// Preamble/thinking text that should be displayed dimmed.
    /// Used by Claude Code backend for conversational text before or after tool output.
    Preamble(String),
    /// Backoff before retry (attempt number, delay in milliseconds).
    Backoff { attempt: u32, delay_ms: u64 },
    /// Retry is starting after backoff completed.
    Retrying { attempt: u32 },
}

/// Action returned by a streaming callback to control the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamAction {
    /// Continue reading the stream.
    Continue,
    /// Abort the stream early (e.g., response prefix is already invalid).
    Abort,
}

/// Callback type for streaming completion events.
/// Returns `StreamAction` to control whether the stream should continue.
pub type StreamCallback = Box<dyn Fn(StreamEvent) -> StreamAction + Send + 'static>;

/// Trait for AI completion backends.
///
/// Implementations handle the specifics of:
/// - Building provider-specific request formats
/// - Sending requests (HTTP, subprocess, etc.)
/// - Parsing provider-specific response formats
///
/// All backends use streaming by default. The callback receives text deltas
/// as they arrive, enabling real-time UI updates.
pub trait Backend: Send + Sync {
    /// Send a completion request with streaming callbacks.
    ///
    /// The callback is invoked for each text delta as tokens arrive.
    /// Returns the final CompletionResponse when complete.
    fn complete_streaming(
        &self,
        request: &CompletionRequest,
        callback: StreamCallback,
    ) -> Result<CompletionResponse, BackendError>;
}