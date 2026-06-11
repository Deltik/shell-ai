//! Native Anthropic Messages API backend.
//!
//! This backend uses the Anthropic Messages API directly, with native
//! structured outputs (`output_config.format`) for JSON schema enforcement.

use super::{
    Backend, BackendError, CompletionRequest, CompletionResponse, HttpStatus, StreamAction,
    StreamCallback, build_history_messages, emit_text_delta, handle_http_status,
    post_json_streaming_retryable, MAX_RETRIES,
};
use serde_json::json;

/// Anthropic API version header value
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Default max tokens for Anthropic (required field)
const DEFAULT_MAX_TOKENS: u32 = 64000;

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
    /// Effort level (e.g., "low", "medium", "high", "xhigh", "max"); sent verbatim
    effort: Option<String>,
}

impl AnthropicBackend {
    /// Create a new Anthropic backend.
    pub fn new(
        base_url: String,
        api_key: Option<String>,
        model: String,
        max_tokens: Option<u32>,
        effort: Option<String>,
    ) -> Self {
        Self {
            base_url,
            api_key,
            model,
            max_tokens,
            effort,
        }
    }

    /// Build the request payload.
    fn build_payload(&self, request: &CompletionRequest, stream: bool) -> serde_json::Value {
        let system_content = request.system_messages.join("\n\n");

        let messages = build_history_messages(request);

        let mut payload = json!({
            "model": &self.model,
            "max_tokens": self.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
            "system": system_content,
            "messages": messages,
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

        // Effort lives in output_config alongside format; indexing creates the
        // object if no schema set it above
        if let Some(ref effort) = self.effort {
            payload["output_config"]["effort"] = json!(effort);
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

        // Retry loop for rate limiting and transient network errors
        for attempt in 0..=MAX_RETRIES {
            let stream = match post_json_streaming_retryable(&url, None, &extra_headers, &payload, attempt, &callback)? {
                Some(s) => s,
                None => continue,
            };

            // Anthropic-specific: detect context length errors in 400 responses
            if stream.status() == 400 {
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

            let mut stream = match handle_http_status(stream, attempt, &callback)? {
                (HttpStatus::Retry, _) => continue,
                (HttpStatus::Ready, s) => s,
            };

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
                                    if emit_text_delta(text, &mut full_content, &callback) == StreamAction::Abort {
                                        return Ok(CompletionResponse { content: full_content, is_truncated: false });
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

#[cfg(test)]
mod tests {
    use super::*;

    fn request(schema: Option<serde_json::Value>) -> CompletionRequest {
        CompletionRequest {
            system_messages: vec![],
            message_history: vec![],
            user_message: "Hello".to_string(),
            json_schema: schema,
            schema_name: "test".to_string(),
        }
    }

    #[test]
    fn test_build_payload_effort_without_schema() {
        let backend = AnthropicBackend::new(
            "https://api.anthropic.com".to_string(),
            Some("sk-ant-test".to_string()),
            "claude-sonnet-4-5".to_string(),
            None,
            Some("medium".to_string()),
        );

        let payload = backend.build_payload(&request(None), false);
        assert_eq!(payload["output_config"]["effort"], "medium");
        assert!(payload["output_config"].get("format").is_none());
    }

    #[test]
    fn test_build_payload_effort_alongside_schema_format() {
        let backend = AnthropicBackend::new(
            "https://api.anthropic.com".to_string(),
            Some("sk-ant-test".to_string()),
            "claude-sonnet-4-5".to_string(),
            None,
            Some("max".to_string()),
        );

        let schema = serde_json::json!({"type": "object", "additionalProperties": false});
        let payload = backend.build_payload(&request(Some(schema.clone())), false);
        assert_eq!(payload["output_config"]["effort"], "max");
        assert_eq!(payload["output_config"]["format"]["schema"], schema);
    }

    #[test]
    fn test_build_payload_no_effort_no_output_config() {
        let backend = AnthropicBackend::new(
            "https://api.anthropic.com".to_string(),
            Some("sk-ant-test".to_string()),
            "claude-sonnet-4-5".to_string(),
            None,
            None,
        );

        let payload = backend.build_payload(&request(None), false);
        assert!(payload.get("output_config").is_none());
    }
}