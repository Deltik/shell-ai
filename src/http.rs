use anyhow::{anyhow, Result};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read};
use std::time::Duration;
use ureq::Proxy;

/// Streaming timeout for SSE connections
const STREAM_TIMEOUT_SECS: u64 = 300;

/// A streaming HTTP response that yields SSE events.
pub struct SseStream {
    reader: BufReader<Box<dyn Read + Send + Sync>>,
    status: u16,
    /// Retry-After header value in seconds (for 429 responses).
    retry_after_secs: Option<u64>,
}

impl SseStream {
    /// Get the HTTP status code.
    pub fn status(&self) -> u16 {
        self.status
    }

    /// Get the Retry-After header value in seconds, if present.
    /// This is typically set on 429 (Too Many Requests) responses.
    pub fn retry_after_secs(&self) -> Option<u64> {
        self.retry_after_secs
    }

    /// Read the next SSE data line.
    /// Returns None when the stream ends.
    /// Filters out non-data lines (comments, event types, etc.)
    pub fn next_data(&mut self) -> Result<Option<String>> {
        let mut line = String::new();

        loop {
            line.clear();
            let bytes_read = self.reader.read_line(&mut line)
                .map_err(|e| anyhow!("Failed to read SSE stream: {}", e))?;

            if bytes_read == 0 {
                return Ok(None); // End of stream
            }

            let line = line.trim();

            // Skip empty lines
            if line.is_empty() {
                continue;
            }

            // Handle data lines (SSE spec: optional space after colon)
            if let Some(data) = line.strip_prefix("data: ").or_else(|| line.strip_prefix("data:"))
            {
                // Check for end of stream marker
                if data == "[DONE]" {
                    return Ok(None);
                }
                return Ok(Some(data.to_string()));
            }

            // Skip other SSE fields (event:, id:, retry:, comments starting with :)
        }
    }

    /// Read the entire response body as a string (for error handling).
    pub fn read_body(mut self) -> Result<String> {
        let mut body = String::new();
        self.reader.read_to_string(&mut body)
            .map_err(|e| anyhow!("Failed to read response body: {}", e))?;
        Ok(body)
    }
}

/// Create a streaming agent with longer timeout.
fn create_streaming_agent() -> ureq::Agent {
    let mut config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(STREAM_TIMEOUT_SECS)))
        .http_status_as_error(false);

    if let Some(proxy) = Proxy::try_from_env() {
        log::debug!("Using proxy from environment for streaming: {:?}", proxy);
        config = config.proxy(Some(proxy));
    }

    config.build().into()
}

/// Send a POST request for SSE streaming.
/// Returns an SseStream for reading events.
pub fn post_json_streaming(
    url: &str,
    bearer_token: Option<&str>,
    extra_headers: &[(&str, &str)],
    body: &Value,
) -> Result<SseStream> {
    let agent = create_streaming_agent();

    let mut request = agent.post(url);

    if let Some(token) = bearer_token {
        request = request.header("Authorization", &format!("Bearer {}", token));
    }

    for (k, v) in extra_headers {
        request = request.header(*k, *v);
    }

    // Add Accept header for SSE
    request = request.header("Accept", "text/event-stream");

    match request.send_json(body) {
        Ok(response) => {
            let status = response.status().as_u16();

            // Parse Retry-After header if present (for 429 responses)
            let retry_after_secs = response
                .headers()
                .get("Retry-After")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());

            let body_reader: Box<dyn Read + Send + Sync> = Box::new(response.into_body().into_reader());
            let reader = BufReader::new(body_reader);
            Ok(SseStream { reader, status, retry_after_secs })
        }
        Err(e) => Err(anyhow!("Network error: {}", e)),
    }
}