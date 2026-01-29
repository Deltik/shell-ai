use anyhow::{anyhow, Result};
use serde_json::Value;
use std::time::Duration;
use ureq::Proxy;

/// Request timeout in seconds
const TIMEOUT_SECS: u64 = 60;

/// Create an HTTP agent with proxy support from environment variables.
///
/// Respects standard proxy environment variables: HTTP_PROXY, HTTPS_PROXY, NO_PROXY
/// (and lowercase variants http_proxy, https_proxy, no_proxy).
fn create_agent() -> ureq::Agent {
    let mut config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(TIMEOUT_SECS)))
        .http_status_as_error(false); // We handle status codes ourselves

    // Try to get proxy from environment variables
    if let Some(proxy) = Proxy::try_from_env() {
        log::debug!("Using proxy from environment: {:?}", proxy);
        config = config.proxy(Some(proxy));
    }

    config.build().into()
}

/// Send a POST request with JSON body and return the response status and body.
/// Respects HTTP_PROXY/HTTPS_PROXY environment variables.
/// Returns (status_code, body_text) on any response, or error on network failure.
pub fn post_json_raw(
    url: &str,
    bearer_token: Option<&str>,
    extra_headers: &[(&str, &str)],
    body: &Value,
) -> Result<(u16, String)> {
    let agent = create_agent();

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