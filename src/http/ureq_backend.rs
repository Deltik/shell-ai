use serde_json::Value;
use std::io::{BufReader, Read};
use std::time::Duration;
use ureq::Proxy;

use super::HttpError;

use super::{SseStream, STREAM_TIMEOUT_SECS};

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

/// Send a POST request for SSE streaming using ureq.
pub fn post_json_streaming(
    url: &str,
    bearer_token: Option<&str>,
    extra_headers: &[(&str, &str)],
    body: &Value,
) -> Result<SseStream, HttpError> {
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

            let body_reader: Box<dyn Read + Send> = Box::new(response.into_body().into_reader());
            let reader = BufReader::new(body_reader);
            Ok(SseStream { reader, status, retry_after_secs })
        }
        Err(e) => Err(HttpError::Network(e.to_string())),
    }
}