//! Native Anthropic Messages API backend.
//!
//! This backend uses the Anthropic Messages API directly, with native
//! structured outputs (`output_config.format`) for JSON schema enforcement.

use super::{Backend, BackendError, CompletionRequest, CompletionResponse, StreamCallback, StreamEvent};
use crate::http;
use serde_json::json;
use std::thread;
use std::time::Duration;

/// Anthropic API version header value
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Default max tokens for Anthropic (required field)
const DEFAULT_MAX_TOKENS: u32 = 64000;

/// Maximum number of retry attempts for rate limiting (HTTP 429).
const MAX_RETRIES: u32 = 5;

/// Base delay for exponential backoff in milliseconds.
const BASE_DELAY_MS: u64 = 1000;

/// Backend for the native Anthropic Messages API.
#[derive(Clone)]
pub struct AnthropicBackend {
    /// Base URL for the Anthropic API
    base_url: String,
    /// API key for x-api-key header authentication (None = omit header)
    api_key: Option<String>,
    /// Model identifier (e.g., "claude-sonnet-4-20250514")
    model: String,
    /// Maximum tokens in the response (required by Anthropic API)
    max_tokens: Option<u32>,
}

impl AnthropicBackend {
    /// Create a new Anthropic backend.
    pub fn new(base_url: String, api_key: Option<String>, model: String, max_tokens: Option<u32>) -> Self {
        Self {
            base_url,
            api_key,
            model,
            max_tokens,
        }
    }

    /// Build the request payload.
    fn build_payload(&self, request: &CompletionRequest, stream: bool) -> serde_json::Value {
        let system_content = request.system_messages.join("\n\n");

        let mut payload = json!({
            "model": &self.model,
            "max_tokens": self.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
            "system": system_content,
            "messages": [
                { "role": "user", "content": &request.user_message }
            ],
            "stream": stream,
        });

        // Use native structured outputs if schema provided
        if let Some(ref schema) = request.json_schema {
            payload["output_config"] = json!({
                "format": {
                    "type": "json_schema",
                    "schema": schema
                }
            });
        }

        payload
    }
}

impl Backend for AnthropicBackend {
    fn complete_streaming(
        &self,
        request: &CompletionRequest,
        callback: StreamCallback,
    ) -> Result<CompletionResponse, BackendError> {
        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
        let payload = self.build_payload(request, true);

        let mut extra_headers = vec![
            ("anthropic-version", ANTHROPIC_VERSION),
        ];
        if let Some(ref key) = self.api_key {
            extra_headers.push(("x-api-key", key.as_str()));
        }

        // Retry loop for rate limiting
        for attempt in 0..=MAX_RETRIES {
            let mut stream = http::post_json_streaming(&url, None, &extra_headers, &payload)
                .map_err(|e| BackendError::NetworkError(e.to_string()))?;

            let status = stream.status();

            // Handle HTTP 429 (rate limiting) with Retry-After header or exponential backoff
            if status == 429 {
                if attempt < MAX_RETRIES {
                    // Use Retry-After header if provided, otherwise exponential backoff
                    let delay_ms = stream
                        .retry_after_secs()
                        .map(|secs| secs * 1000)
                        .unwrap_or_else(|| BASE_DELAY_MS * (1 << attempt));
                    callback(StreamEvent::Backoff { attempt: attempt + 1, delay_ms });
                    thread::sleep(Duration::from_millis(delay_ms));
                    callback(StreamEvent::Retrying { attempt: attempt + 1 });
                    continue;
                } else {
                    let body = stream.read_body().unwrap_or_default().trim().to_string();
                    return Err(BackendError::RateLimited(
                        if body.is_empty() { "Too many requests".to_string() } else { body }
                    ));
                }
            }

            // Handle HTTP errors
            if status == 413 {
                let body = stream.read_body().unwrap_or_default().trim().to_string();
                return Err(BackendError::RequestTooLarge(
                    if body.is_empty() { "context length exceeded".to_string() } else { body }
                ));
            }

            if status == 400 {
                let body = stream.read_body().unwrap_or_default().trim().to_string();
                if let Ok(resp_json) = serde_json::from_str::<serde_json::Value>(&body) {
                    if let Some(error_type) = resp_json.get("error")
                        .and_then(|e| e.get("type"))
                        .and_then(|t| t.as_str())
                    {
                        if error_type == "invalid_request_error" {
                            if let Some(msg) = resp_json.get("error")
                                .and_then(|e| e.get("message"))
                                .and_then(|m| m.as_str())
                            {
                                if msg.contains("context length") || msg.contains("too long") {
                                    return Err(BackendError::RequestTooLarge(msg.to_string()));
                                }
                            }
                        }
                    }
                }
                return Err(BackendError::ApiError(format!("HTTP 400: {}", body)));
            }

            if !(200..300).contains(&status) {
                let body = stream.read_body().unwrap_or_default().trim().to_string();
                return Err(BackendError::ApiError(format!(
                    "HTTP {}: {}",
                    status,
                    if body.is_empty() { "Unknown error".to_string() } else { body }
                )));
            }

            // Process SSE stream
            // With structured outputs, JSON responses arrive as text_delta events
            let mut full_content = String::new();
            let mut is_truncated = false;

            while let Some(data) = stream.next_data().map_err(|e| BackendError::NetworkError(e.to_string()))? {
                let event: serde_json::Value = match serde_json::from_str(&data) {
                    Ok(v) => v,
                    Err(e) => {
                        log::debug!("Failed to parse Anthropic SSE chunk: {} - data: {}", e, data);
                        continue;
                    }
                };

                let event_type = event.get("type").and_then(|t| t.as_str()).unwrap_or("");

                match event_type {
                    "content_block_delta" => {
                        if let Some(delta) = event.get("delta") {
                            if delta.get("type").and_then(|t| t.as_str()) == Some("text_delta") {
                                if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                                    if !text.is_empty() {
                                        full_content.push_str(text);
                                        callback(StreamEvent::TextDelta(text.to_string()));
                                    }
                                }
                            }
                        }
                    }
                    "message_delta" => {
                        if let Some(delta) = event.get("delta") {
                            if let Some(stop_reason) = delta.get("stop_reason").and_then(|r| r.as_str()) {
                                if stop_reason == "max_tokens" {
                                    is_truncated = true;
                                }
                            }
                        }
                    }
                    "error" => {
                        let msg = event.get("error")
                            .and_then(|e| e.get("message"))
                            .and_then(|m| m.as_str())
                            .unwrap_or("Unknown error");
                        return Err(BackendError::ApiError(msg.to_string()));
                    }
                    _ => {}
                }
            }

            return Ok(CompletionResponse {
                content: full_content,
                is_truncated,
            });
        }

        // This should be unreachable, but just in case
        Err(BackendError::RateLimited("Max retries exceeded".to_string()))
    }
}