//! OpenAI-compatible backend for HTTP-based AI providers.
//!
//! This backend works with:
//! - OpenAI API
//! - Azure OpenAI
//! - Groq
//! - Mistral
//! - Ollama
//! - Any OpenAI-compatible API

use super::{
    Backend, BackendError, CompletionRequest, CompletionResponse, HttpStatus, StreamAction,
    StreamCallback, build_history_messages, emit_text_delta, handle_http_status, MAX_RETRIES,
};
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

    /// Build the request payload.
    fn build_payload(&self, request: &CompletionRequest, stream: bool) -> serde_json::Value {
        let mut messages: Vec<serde_json::Value> = Vec::new();

        for sys_msg in &request.system_messages {
            messages.push(json!({"role": "system", "content": sys_msg}));
        }

        // History + final user message
        messages.extend(build_history_messages(request));

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
            let stream = http::post_json_streaming(&self.url, bearer_token, &extra_headers, &payload)
                .map_err(|e| BackendError::NetworkError(e.to_string()))?;

            let mut stream = match handle_http_status(stream, attempt, &callback)? {
                (HttpStatus::Retry, _) => continue,
                (HttpStatus::Ready, s) => s,
            };

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
                            if emit_text_delta(content, &mut full_content, &callback) == StreamAction::Abort {
                                return Ok(CompletionResponse { content: full_content, is_truncated: false });
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