//! OpenAI-compatible backend for HTTP-based AI providers.
//!
//! This backend works with:
//! - OpenAI API
//! - Azure OpenAI
//! - Groq
//! - Mistral
//! - Ollama
//! - Any OpenAI-compatible API

use super::{Backend, BackendError, CompletionRequest, CompletionResponse, StreamCallback, StreamEvent};
use crate::http;
use serde_json::json;
use std::thread;
use std::time::Duration;

/// Maximum number of retry attempts for rate limiting (HTTP 429).
const MAX_RETRIES: u32 = 5;

/// Base delay for exponential backoff in milliseconds.
const BASE_DELAY_MS: u64 = 1000;

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

    /// Build the request payload.
    fn build_payload(&self, request: &CompletionRequest, stream: bool) -> serde_json::Value {
        let mut messages: Vec<serde_json::Value> = Vec::new();

        // Merge all system messages into one with \n\n separator (required for Jinja-based models)
        if !request.system_messages.is_empty() {
            let merged_system = request.system_messages.join("\n\n");
            messages.push(json!({"role": "system", "content": merged_system}));
        }
        messages.push(json!({"role": "user", "content": &request.user_message}));

        let mut payload = json!({
            "model": &self.model,
            "messages": messages,
            "temperature": self.temperature,
            "stream": stream,
        });

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

        if let Some(max_tokens) = self.max_tokens {
            payload["max_tokens"] = json!(max_tokens);
        }

        payload
    }
}

impl Backend for OpenAiBackend {
    fn complete_streaming(
        &self,
        request: &CompletionRequest,
        callback: StreamCallback,
    ) -> Result<CompletionResponse, BackendError> {
        let payload = self.build_payload(request, true);
        let bearer_token = self.api_key.as_deref();
        let extra_headers = self.extra_headers_ref();

        // Retry loop for rate limiting
        for attempt in 0..=MAX_RETRIES {
            let mut stream = http::post_json_streaming(&self.url, bearer_token, &extra_headers, &payload)
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

            if !(200..300).contains(&status) {
                let body = stream.read_body().unwrap_or_default().trim().to_string();
                return Err(BackendError::ApiError(format!(
                    "HTTP {}: {}",
                    status,
                    if body.is_empty() { "Unknown error".to_string() } else { body }
                )));
            }

            // Process SSE stream
            let mut full_content = String::new();
            let mut is_truncated = false;

            while let Some(data) = stream.next_data().map_err(|e| BackendError::NetworkError(e.to_string()))? {
                // Parse the SSE data as JSON
                let chunk: serde_json::Value = match serde_json::from_str(&data) {
                    Ok(v) => v,
                    Err(e) => {
                        log::debug!("Failed to parse SSE chunk: {} - data: {}", e, data);
                        continue;
                    }
                };

                // Check for error in chunk
                if let Some(error) = chunk.get("error") {
                    let msg = error.get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("Unknown error");
                    return Err(BackendError::ApiError(msg.to_string()));
                }

                // Extract delta content from choices[0].delta.content
                if let Some(choices) = chunk.get("choices").and_then(|c| c.as_array()) {
                    for choice in choices {
                        // Check finish reason
                        if let Some(finish_reason) = choice.get("finish_reason").and_then(|r| r.as_str()) {
                            if finish_reason == "length" {
                                is_truncated = true;
                            }
                        }

                        // Extract delta content
                        if let Some(content) = choice.get("delta")
                            .and_then(|d| d.get("content"))
                            .and_then(|c| c.as_str())
                        {
                            if !content.is_empty() {
                                full_content.push_str(content);
                                callback(StreamEvent::TextDelta(content.to_string()));
                            }
                        }
                    }
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

    #[test]
    fn test_build_payload_merges_system_messages() {
        let backend = OpenAiBackend::new(
            "https://api.openai.com/v1/chat/completions".to_string(),
            "gpt-4".to_string(),
            Some("sk-test".to_string()),
            vec![],
            0.5,
            Some(1000),
        );

        let request = CompletionRequest {
            system_messages: vec![
                "First system message".to_string(),
                "Second system message".to_string(),
                "Third system message".to_string(),
            ],
            user_message: "Hello".to_string(),
            json_schema: None,
            schema_name: "test".to_string(),
        };

        let payload = backend.build_payload(&request, false);
        let messages = &payload["messages"];

        // Should have exactly 2 messages: 1 system + 1 user
        assert_eq!(messages.as_array().unwrap().len(), 2);

        // First message should be system with all content merged
        let system_msg = &messages.as_array().unwrap()[0];
        assert_eq!(system_msg["role"], "system");
        let expected_content = "First system message\n\nSecond system message\n\nThird system message";
        assert_eq!(system_msg["content"], expected_content);

        // Second message should be user
        let user_msg = &messages.as_array().unwrap()[1];
        assert_eq!(user_msg["role"], "user");
        assert_eq!(user_msg["content"], "Hello");
    }

    #[test]
    fn test_build_payload_single_system_message() {
        let backend = OpenAiBackend::new(
            "https://api.openai.com/v1/chat/completions".to_string(),
            "gpt-4".to_string(),
            Some("sk-test".to_string()),
            vec![],
            0.5,
            Some(1000),
        );

        let request = CompletionRequest {
            system_messages: vec!["Single system message".to_string()],
            user_message: "Hello".to_string(),
            json_schema: None,
            schema_name: "test".to_string(),
        };

        let payload = backend.build_payload(&request, false);
        let messages = &payload["messages"];

        // Should have exactly 2 messages: 1 system + 1 user
        assert_eq!(messages.as_array().unwrap().len(), 2);

        let system_msg = &messages.as_array().unwrap()[0];
        assert_eq!(system_msg["role"], "system");
        assert_eq!(system_msg["content"], "Single system message");
    }

    #[test]
    fn test_build_payload_no_system_messages() {
        let backend = OpenAiBackend::new(
            "https://api.openai.com/v1/chat/completions".to_string(),
            "gpt-4".to_string(),
            Some("sk-test".to_string()),
            vec![],
            0.5,
            Some(1000),
        );

        let request = CompletionRequest {
            system_messages: vec![],
            user_message: "Hello".to_string(),
            json_schema: None,
            schema_name: "test".to_string(),
        };

        let payload = backend.build_payload(&request, false);
        let messages = &payload["messages"];

        // Should have exactly 1 message: user only
        assert_eq!(messages.as_array().unwrap().len(), 1);

        let user_msg = &messages.as_array().unwrap()[0];
        assert_eq!(user_msg["role"], "user");
        assert_eq!(user_msg["content"], "Hello");
    }
}
