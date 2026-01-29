//! Native Anthropic Messages API backend.
//!
//! This backend uses the Anthropic Messages API directly, with tool use
//! for structured output enforcement.

use super::{Backend, BackendError, CompletionRequest, CompletionResponse};
use crate::http;
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
}

impl Backend for AnthropicBackend {
    fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse, BackendError> {
        let url = format!(
            "{}/v1/messages",
            self.base_url.trim_end_matches('/')
        );

        // Combine system messages into a single system field
        // Anthropic uses a top-level "system" field, not system role messages
        let system_content = request.system_messages.join("\n\n");

        // Build the base payload
        let mut payload = json!({
            "model": &self.model,
            "max_tokens": self.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
            "system": system_content,
            "messages": [
                { "role": "user", "content": &request.user_message }
            ],
        });

        // Use tool use for structured output if schema provided
        // This forces the model to respond with valid JSON matching the schema
        if let Some(ref schema) = request.json_schema {
            payload["tools"] = json!([{
                "name": &request.schema_name,
                "description": "Output structured JSON response matching the schema",
                "input_schema": schema
            }]);
            payload["tool_choice"] = json!({
                "type": "tool",
                "name": &request.schema_name
            });
        }

        // Anthropic uses x-api-key header, not Bearer token
        let mut extra_headers = vec![
            ("anthropic-version", ANTHROPIC_VERSION),
        ];
        if let Some(ref key) = self.api_key {
            extra_headers.push(("x-api-key", key.as_str()));
        }

        // Use post_json_raw for detailed error handling
        let (status, body) = http::post_json_raw(&url, None, &extra_headers, &payload)
            .map_err(|e| BackendError::NetworkError(e.to_string()))?;

        // Handle 413 Request Entity Too Large
        if status == 413 {
            return Err(BackendError::RequestTooLarge(
                if body.is_empty() {
                    "context length exceeded".to_string()
                } else {
                    body
                },
            ));
        }

        // Anthropic uses 400 for context length exceeded with specific error type
        if status == 400 {
            if let Ok(resp_json) = serde_json::from_str::<serde_json::Value>(&body) {
                if let Some(error_type) = resp_json
                    .get("error")
                    .and_then(|e| e.get("type"))
                    .and_then(|t| t.as_str())
                {
                    if error_type == "invalid_request_error" {
                        if let Some(msg) = resp_json
                            .get("error")
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
        }

        // Handle other HTTP errors
        if status < 200 || status >= 300 {
            return Err(BackendError::ApiError(format!(
                "HTTP {}: {}",
                status,
                if body.is_empty() {
                    "Unknown error".to_string()
                } else {
                    body
                }
            )));
        }

        // Parse response JSON
        let resp_json: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| BackendError::ParseError(format!("failed to parse API response: {}", e)))?;

        // Check for Anthropic error format
        if let Some(error) = resp_json.get("error") {
            let msg = error
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("Unknown error");
            return Err(BackendError::ApiError(msg.to_string()));
        }

        // Extract content based on whether we expect tool use or text
        let content = extract_anthropic_content(&resp_json, request.json_schema.is_some())?;

        // Check for truncation
        let stop_reason = resp_json
            .get("stop_reason")
            .and_then(|r| r.as_str())
            .unwrap_or("");
        let is_truncated = stop_reason == "max_tokens";

        Ok(CompletionResponse {
            content,
            is_truncated,
        })
    }
}

/// Extract content from Anthropic response.
///
/// If expecting tool use (structured output), extracts from `content[].input`.
/// Otherwise, extracts from `content[].text`.
fn extract_anthropic_content(resp: &serde_json::Value, expect_tool_use: bool) -> Result<String, BackendError> {
    let content_array = resp
        .get("content")
        .and_then(|c| c.as_array())
        .ok_or_else(|| BackendError::ParseError("Missing content array in response".to_string()))?;

    if expect_tool_use {
        // Look for tool_use block and extract the input as JSON string
        for block in content_array {
            if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                if let Some(input) = block.get("input") {
                    return serde_json::to_string(input)
                        .map_err(|e| BackendError::ParseError(format!("Failed to serialize tool input: {}", e)));
                }
            }
        }
        Err(BackendError::ParseError("No tool_use block found in response".to_string()))
    } else {
        // Look for text block
        for block in content_array {
            if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    return Ok(text.to_string());
                }
            }
        }
        Err(BackendError::ParseError("No text block found in response".to_string()))
    }
}