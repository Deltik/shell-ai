mod curl_backend;
mod curl_cmd_backend;
mod curl_ffi;
mod ureq_backend;

use anyhow::{anyhow, Result};
use serde_json::Value;
use std::fmt;
use std::io::{BufRead, BufReader, Read};
use std::sync::OnceLock;

use crate::config::FieldMeta;
use curl_ffi::CurlLibrary;

/// Errors from the HTTP layer.
#[derive(Debug)]
pub enum HttpError {
    /// Transient network error (connection refused, timeout, TLS failure, etc.).
    /// These are worth retrying.
    Network(String),
    /// Configuration error (bad binary path, invalid command, etc.).
    /// These are permanent and should not be retried.
    Config(String),
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HttpError::Network(msg) => write!(f, "{}", msg),
            HttpError::Config(msg) => write!(f, "{}", msg),
        }
    }
}

impl std::error::Error for HttpError {}

/// Streaming timeout for SSE connections
const STREAM_TIMEOUT_SECS: u64 = 300;

/// User-configured curl command with its config metadata for error messages.
struct CurlCmdConfig {
    cmd: String,
    meta: &'static FieldMeta,
}

static CURL_CMD: OnceLock<Option<CurlCmdConfig>> = OnceLock::new();

/// Cached result of attempting to load libcurl-impersonate at runtime.
static CURL_LIB: OnceLock<Option<CurlLibrary>> = OnceLock::new();

/// Set the curl command from config. Call once from main.rs after config loads.
pub fn set_curl_cmd(cmd: Option<String>, meta: Option<&'static FieldMeta>) {
    let _ = CURL_CMD.set(cmd.zip(meta).map(|(cmd, meta)| CurlCmdConfig { cmd, meta }));
}

/// A streaming HTTP response that yields SSE events.
pub struct SseStream {
    reader: BufReader<Box<dyn Read + Send>>,
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

/// Send a POST request for SSE streaming.
/// Returns an SseStream for reading events.
///
/// Backend selection priority:
/// 1. SHAI_CURL config → curl-compatible subprocess
/// 2. libcurl-impersonate detected via dlopen → library backend
/// 3. ureq → built-in default
pub fn post_json_streaming(
    url: &str,
    bearer_token: Option<&str>,
    extra_headers: &[(&str, &str)],
    body: &Value,
) -> Result<SseStream, HttpError> {
    // Priority 1: User-configured curl binary
    let curl_cmd = CURL_CMD.get_or_init(|| None);
    if let Some(config) = curl_cmd {
        return curl_cmd_backend::post_json_streaming(
            &config.cmd, config.meta, url, bearer_token, extra_headers, body,
        );
    }

    // Priority 2: Runtime-detected libcurl-impersonate
    let curl_lib = CURL_LIB.get_or_init(|| {
        match curl_ffi::try_load() {
            Ok(lib) => {
                log::debug!("Using libcurl with impersonation support");
                Some(lib)
            }
            Err(e) => {
                log::debug!("libcurl-impersonate not available: {}", e);
                None
            }
        }
    });

    if let Some(curl) = curl_lib {
        return curl_backend::post_json_streaming(curl, url, bearer_token, extra_headers, body);
    }

    // Priority 3: Built-in ureq
    ureq_backend::post_json_streaming(url, bearer_token, extra_headers, body)
}