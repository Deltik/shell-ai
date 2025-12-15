use anyhow::{anyhow, Result};
use serde_json::Value;
use std::time::Duration;
use ureq::Proxy;

/// Maximum number of retry attempts for transient errors
const MAX_RETRIES: u32 = 3;

/// Initial backoff delay in milliseconds
const INITIAL_BACKOFF_MS: u64 = 1000;

/// Request timeout in seconds
const TIMEOUT_SECS: u64 = 60;

/// Create an HTTP agent with proxy support from environment variables.
///
/// Respects standard proxy environment variables: HTTP_PROXY, HTTPS_PROXY, NO_PROXY
/// (and lowercase variants http_proxy, https_proxy, no_proxy).
fn create_agent(http_status_as_error: bool) -> ureq::Agent {
    let mut config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(TIMEOUT_SECS)))
        .http_status_as_error(http_status_as_error);

    // Try to get proxy from environment variables
    if let Some(proxy) = Proxy::try_from_env() {
        log::debug!("Using proxy from environment: {:?}", proxy);
        config = config.proxy(Some(proxy));
    }

    config.build().into()
}

/// Get a human-friendly description for HTTP status codes
fn status_description(status: u16) -> &'static str {
    match status {
        401 => "Unauthorized - check your API key",
        403 => "Forbidden - check your API key permissions",
        404 => "Not found - check the API endpoint URL",
        429 => "Rate limited - too many requests",
        500..=599 => "Server error - the API is having issues",
        _ => "HTTP error",
    }
}

/// Send a POST request with JSON body and return parsed JSON response.
/// Includes exponential backoff retry for 429 and 5xx errors.
/// Respects HTTP_PROXY/HTTPS_PROXY environment variables.
pub fn post_json(
    url: &str,
    bearer_token: Option<&str>,
    extra_headers: &[(&str, &str)],
    body: &Value,
) -> Result<Value> {
    let agent = create_agent(true);

    let mut backoff_ms = INITIAL_BACKOFF_MS;

    for attempt in 0..=MAX_RETRIES {
        let mut request = agent.post(url);

        if let Some(token) = bearer_token {
            request = request.header("Authorization", &format!("Bearer {}", token));
        }

        for (k, v) in extra_headers {
            request = request.header(*k, *v);
        }

        return match request.send_json(body) {
            Ok(response) => {
                let body_str = response.into_body().read_to_string()?;
                let json: Value = serde_json::from_str(&body_str)
                    .map_err(|e| anyhow!("Failed to parse JSON: {}", e))?;
                Ok(json)
            }
            Err(ureq::Error::StatusCode(status)) => {
                // Rate limit (429) or server error (5xx) - retry with backoff
                if status == 429 || (500..600).contains(&status) {
                    if attempt < MAX_RETRIES {
                        log::warn!(
                            "{} (HTTP {}) - attempt {}/{}, retrying in {}ms...",
                            status_description(status),
                            status,
                            attempt + 1,
                            MAX_RETRIES + 1,
                            backoff_ms
                        );
                        std::thread::sleep(Duration::from_millis(backoff_ms));
                        backoff_ms *= 2;
                        continue;
                    }
                }

                Err(anyhow!("HTTP {}: {}", status, status_description(status)))
            }
            Err(e) => {
                // Network error - retry
                if attempt < MAX_RETRIES {
                    log::warn!(
                        "Network error (attempt {}/{}): {}, retrying in {}ms...",
                        attempt + 1,
                        MAX_RETRIES + 1,
                        e,
                        backoff_ms
                    );
                    std::thread::sleep(Duration::from_millis(backoff_ms));
                    backoff_ms *= 2;
                    continue;
                }
                Err(anyhow!("Network error: {}", e))
            }
        }
    }

    Err(anyhow!("Max retries exceeded"))
}

/// Send a POST request with JSON body and return the response status and body.
/// Does NOT retry - caller handles retry logic.
/// Respects HTTP_PROXY/HTTPS_PROXY environment variables.
/// Returns (status_code, body_text) on any response, or error on network failure.
pub fn post_json_raw(
    url: &str,
    bearer_token: Option<&str>,
    extra_headers: &[(&str, &str)],
    body: &Value,
) -> Result<(u16, String)> {
    // Use create_agent with http_status_as_error=false to get response body for all status codes
    let agent = create_agent(false);

    let mut request = agent.post(url);

    if let Some(token) = bearer_token {
        request = request.header("Authorization", &format!("Bearer {}", token));
    }

    for (k, v) in extra_headers {
        request = request.header(*k, *v);
    }

    match request.send_json(body) {
        Ok(response) => {
            let status = response.status().as_u16();
            let body_str = response
                .into_body()
                .read_to_string()
                .map_err(|e| anyhow!("Failed to read response body: {}", e))?;
            Ok((status, body_str))
        }
        Err(e) => Err(anyhow!("Network error: {}", e)),
    }
}

// ============================================================================
// API Response Utilities
// ============================================================================

/// Extract the content string from an OpenAI-compatible chat completion response.
///
/// Looks for `choices[0].message.content` in the response JSON.
pub fn extract_content_from_response(resp_json: &Value) -> Result<&str> {
    resp_json
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .ok_or_else(|| anyhow!("API response missing choices[0].message.content"))
}

/// Check if the response was truncated due to max_tokens limit.
///
/// Returns `true` if `choices[0].finish_reason` is "length",
/// indicating the response was cut off before completion.
pub fn is_truncated(resp_json: &Value) -> bool {
    resp_json
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("finish_reason"))
        .and_then(|r| r.as_str())
        .map(|r| r == "length")
        .unwrap_or(false)
}

/// Extract an error message from an API error response, if present.
///
/// Looks for `error.message` or `error.error` in the response JSON.
/// Returns `None` if no error field is found.
pub fn extract_api_error(resp_json: &Value) -> Option<String> {
    resp_json.get("error").and_then(|err| {
        err.get("message")
            .or_else(|| err.get("error"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    })
}