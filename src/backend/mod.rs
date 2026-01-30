//! Backend abstraction layer for AI completion providers.
//!
//! This module provides a trait-based abstraction over different AI backends:
//! - OpenAI-compatible HTTP APIs (OpenAI, Groq, Azure, Ollama, Mistral)
//! - Native Anthropic Messages API
//! - Claude Code CLI (subprocess-based)

mod anthropic;
mod claude_code;
mod openai;

pub use anthropic::AnthropicBackend;
pub use claude_code::ClaudeCodeBackend;
pub use openai::OpenAiBackend;

use anyhow::Result;

/// A completion request to be sent to an AI backend.
#[derive(Debug, Clone)]
pub struct CompletionRequest {
    /// System messages (instructions, documentation, etc.)
    /// For OpenAI-compatible: sent as multiple system role messages
    /// For Anthropic: concatenated or sent as system field
    /// For Claude Code: combined into the prompt
    pub system_messages: Vec<String>,

    /// The user's message/prompt
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

/// Callback type for streaming completion events.
pub type StreamCallback = Box<dyn Fn(StreamEvent) + Send + 'static>;

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

    /// Convenience method for non-streaming use cases.
    /// Collects all tokens and returns the final response.
    fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse, BackendError> {
        self.complete_streaming(request, Box::new(|_| {}))
    }
}