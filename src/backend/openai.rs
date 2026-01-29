//! OpenAI-compatible backend for HTTP-based AI providers.
//!
//! This backend works with:
//! - OpenAI API
//! - Azure OpenAI
//! - Groq
//! - Mistral
//! - Ollama
//! - Any OpenAI-compatible API

use super::{Backend, BackendError, CompletionRequest, CompletionResponse};
use crate::http;
use serde_json::json;

/// Backend for OpenAI-compatible HTTP APIs.
#[derive(Clone)]
pub struct OpenAiBackend {
    /// Full URL for chat completions endpoint
    url: String,
    /// Model identifier
    model: String,
    /// API key for Bearer token authentication
    api_key: Option<String>,
    /// Extra headers (e.g., Azure's api-key, OpenAI's Organization)
    extra_headers: Vec<(String, String)>,
    /// Sampling temperature (0.0 to 1.0)
    temperature: f32,
    /// Maximum tokens in the response
    max_tokens: Option<u32>,
}

impl OpenAiBackend {
    /// Create a new OpenAI-compatible backend.
    pub fn new(
        url: String,
        model: String,
        api_key: Option<String>,
        extra_headers: Vec<(String, String)>,
        temperature: f32,
        max_tokens: Option<u32>,
    ) -> Self {
        Self {
            url,
            model,
            api_key,
            extra_headers,
            temperature,
            max_tokens,
        }
    }

    /// Get extra headers as borrowed string slices for use with http functions.
    fn extra_headers_ref(&self) -> Vec<(&str, &str)> {
        self.extra_headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect()
    }
}

impl Backend for OpenAiBackend {
    fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse, BackendError> {
        // Build messages array
        let mut messages: Vec<serde_json::Value> = Vec::new();

        // Add system messages
        for sys_msg in &request.system_messages {
            messages.push(json!({"role": "system", "content": sys_msg}));
        }

        // Add user message
        messages.push(json!({"role": "user", "content": &request.user_message}));

        // Build payload
        let mut payload = json!({
            "model": &self.model,
            "messages": messages,
            "temperature": self.temperature,
        });

        // Add JSON schema for structured output if provided
        if let Some(ref schema) = request.json_schema {
            payload["response_format"] = json!({
                "type": "json_schema",
                "json_schema": {
                    "name": &request.schema_name,
                    "strict": true,
                    "schema": schema
                }
            });
        }

        // Add max_tokens if configured
        if let Some(max_tokens) = self.max_tokens {
            payload["max_tokens"] = json!(max_tokens);
        }

        let bearer_token = self.api_key.as_deref();
        let extra_headers = self.extra_headers_ref();

        // Use post_json_raw to get status code for 413 detection
        let (status, body) = http::post_json_raw(&self.url, bearer_token, &extra_headers, &payload)
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

        // Check for API error in response body
        if let Some(msg) = http::extract_api_error(&resp_json) {
            return Err(BackendError::ApiError(msg));
        }

        // Extract content from response
        let content = http::extract_content_from_response(&resp_json)
            .map_err(|e| BackendError::ParseError(e.to_string()))?;

        let is_truncated = http::is_truncated(&resp_json);

        Ok(CompletionResponse {
            content: content.to_string(),
            is_truncated,
        })
    }
}