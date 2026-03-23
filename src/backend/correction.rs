//! Tolerant completion harness with automatic correction retries.
//!
//! When a model's response fails to parse (e.g., markdown fences around JSON,
//! schema violations), this harness feeds the error back to the model and
//! retries, building up a multi-turn conversation until the response is valid
//! or retries are exhausted.

use std::sync::Mutex;

use super::{
    Backend, BackendError, CompletionRequest, CompletionResponse, HistoryMessage, MessageRole,
    StreamAction, StreamCallback, StreamEvent,
};

/// Validates that a streaming response prefix can conform to the expected JSON schema.
///
/// Currently checks only that the first non-whitespace character matches the schema's
/// root type (e.g., `{` for objects). This catches markdown fences, prose preambles,
/// and other wrapping that makes the response irrecoverably invalid.
struct PrefixValidator {
    expected_start: char,
    decided: bool,
}

impl PrefixValidator {
    /// Create a validator from a JSON schema, if the root type is known.
    fn from_schema(schema: &serde_json::Value) -> Option<Self> {
        let expected_start = match schema.get("type")?.as_str()? {
            "object" => '{',
            "array" => '[',
            _ => return None,
        };
        Some(Self { expected_start, decided: false })
    }

    /// Feed a text chunk. Returns `Abort` if the prefix is already invalid.
    fn feed(&mut self, text: &str) -> StreamAction {
        if self.decided {
            return StreamAction::Continue;
        }
        for ch in text.chars() {
            if ch.is_whitespace() {
                continue;
            }
            self.decided = true;
            return if ch == self.expected_start {
                StreamAction::Continue
            } else {
                StreamAction::Abort
            };
        }
        StreamAction::Continue // all whitespace so far
    }
}

/// Wrap a callback with prefix validation. On the first non-whitespace character
/// that doesn't match the schema's root type, returns `Abort` without calling
/// the inner callback (so invalid content doesn't pollute the UI).
fn validated_callback(
    inner: StreamCallback,
    schema: Option<&serde_json::Value>,
) -> StreamCallback {
    let validator = schema.and_then(PrefixValidator::from_schema);
    match validator {
        None => inner,
        Some(v) => {
            let validator = Mutex::new(v);
            Box::new(move |event| {
                let abort = if let StreamEvent::TextDelta(ref text) = event {
                    validator.lock().unwrap().feed(text) == StreamAction::Abort
                } else {
                    false
                };
                // Always call inner so chars get counted, even on abort
                let result = inner(event);
                if abort { StreamAction::Abort } else { result }
            })
        }
    }
}

/// Metadata about the current correction attempt, passed to the callback factory
/// so callers can adjust UI (e.g., show "Retrying 1/2...").
pub struct CorrectionAttempt {
    /// 0 for the first try, 1+ for correction retries.
    pub attempt: u32,
    /// Maximum number of correction retries (not counting the first attempt).
    pub max_retries: u32,
}

/// Attempt a streaming completion with automatic correction on parse failure.
///
/// On the first attempt, calls the backend normally. If `parse` returns an error,
/// the failed response and error are fed back to the model as conversation history,
/// and the backend is called again with a correction prompt. This repeats up to
/// `max_retries` times.
///
/// # Arguments
/// * `backend` — The AI backend to use.
/// * `request` — The original completion request.
/// * `max_retries` — Maximum correction attempts (0 = no retries, fail immediately).
/// * `make_callback` — Factory that creates a fresh `StreamCallback` per attempt.
/// * `parse` — Parses the response content into `T`. Returns `Err(description)` on failure.
pub fn complete_with_correction<T>(
    backend: &dyn Backend,
    request: &CompletionRequest,
    max_retries: u32,
    mut make_callback: impl FnMut(CorrectionAttempt) -> StreamCallback,
    parse: impl Fn(&CompletionResponse) -> Result<T, String>,
) -> Result<T, BackendError> {
    // First attempt
    let callback = validated_callback(
        make_callback(CorrectionAttempt { attempt: 0, max_retries }),
        request.json_schema.as_ref(),
    );
    let resp = backend.complete_streaming(request, callback)?;

    match parse(&resp) {
        Ok(value) => Ok(value),
        Err(error) if max_retries == 0 => Err(BackendError::ParseError(error)),
        Err(error) => retry_loop(backend, request, &resp, error, max_retries, make_callback, parse),
    }
}

fn build_correction_message(error: &str) -> String {
    format!(
        "Your previous response could not be parsed as valid JSON conforming to the required schema.\n\
         \n\
         Error: {error}\n\
         \n\
         Respond with ONLY the raw JSON object. No Markdown code fences, no explanation, no extra text."
    )
}

fn retry_loop<T>(
    backend: &dyn Backend,
    original_request: &CompletionRequest,
    first_response: &CompletionResponse,
    first_error: String,
    max_retries: u32,
    mut make_callback: impl FnMut(CorrectionAttempt) -> StreamCallback,
    parse: impl Fn(&CompletionResponse) -> Result<T, String>,
) -> Result<T, BackendError> {
    let mut history = original_request.message_history.clone();
    history.push(HistoryMessage {
        role: MessageRole::User,
        content: original_request.user_message.clone(),
    });
    history.push(HistoryMessage {
        role: MessageRole::Assistant,
        content: first_response.content.clone(),
    });

    let mut last_error = first_error;

    for attempt in 1..=max_retries {
        let correction = build_correction_message(&last_error);

        let retry_request = CompletionRequest {
            system_messages: original_request.system_messages.clone(),
            message_history: history.clone(),
            user_message: correction.clone(),
            json_schema: original_request.json_schema.clone(),
            schema_name: original_request.schema_name.clone(),
        };

        let callback = validated_callback(
            make_callback(CorrectionAttempt { attempt, max_retries }),
            original_request.json_schema.as_ref(),
        );

        let resp = backend.complete_streaming(&retry_request, callback)?;

        match parse(&resp) {
            Ok(value) => return Ok(value),
            Err(error) => {
                history.push(HistoryMessage {
                    role: MessageRole::User,
                    content: correction,
                });
                history.push(HistoryMessage {
                    role: MessageRole::Assistant,
                    content: resp.content.clone(),
                });
                last_error = error;
            }
        }
    }

    Err(BackendError::ParseError(format!(
        "Failed after {} correction attempt(s). Last error: {}",
        max_retries, last_error
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// A mock backend that returns a sequence of canned responses.
    /// Records every request it receives for assertion.
    struct MockBackend {
        responses: Mutex<Vec<Result<CompletionResponse, BackendError>>>,
        call_count: Mutex<usize>,
        requests: Mutex<Vec<CompletionRequest>>,
    }

    impl MockBackend {
        fn new(responses: Vec<Result<CompletionResponse, BackendError>>) -> Self {
            Self {
                responses: Mutex::new(responses),
                call_count: Mutex::new(0),
                requests: Mutex::new(Vec::new()),
            }
        }

        fn call_count(&self) -> usize {
            *self.call_count.lock().unwrap()
        }

        fn requests(&self) -> Vec<CompletionRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl Backend for MockBackend {
        fn complete_streaming(
            &self,
            request: &CompletionRequest,
            _callback: StreamCallback,
        ) -> Result<CompletionResponse, BackendError> {
            let mut count = self.call_count.lock().unwrap();
            let responses = self.responses.lock().unwrap();
            self.requests.lock().unwrap().push(request.clone());
            let idx = *count;
            *count += 1;
            if idx < responses.len() {
                responses[idx].clone()
            } else {
                panic!("MockBackend: no response for call #{idx}");
            }
        }
    }

    fn make_request(user_message: &str) -> CompletionRequest {
        CompletionRequest {
            system_messages: vec!["You are helpful.".to_string()],
            message_history: vec![],
            user_message: user_message.to_string(),
            json_schema: None,
            schema_name: "test".to_string(),
        }
    }

    fn noop_callback(_: CorrectionAttempt) -> StreamCallback {
        Box::new(|_| StreamAction::Continue)
    }

    fn parse_command(resp: &CompletionResponse) -> Result<String, String> {
        let v: serde_json::Value = serde_json::from_str(&resp.content)
            .map_err(|e| format!("{e}"))?;
        v.get("command")
            .and_then(|c| c.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| "missing 'command' field".to_string())
    }

    // -- Success on first attempt --

    #[test]
    fn test_first_attempt_success() {
        let backend = MockBackend::new(vec![
            Ok(CompletionResponse {
                content: r#"{"command":"ls -la"}"#.to_string(),
                is_truncated: false,
            }),
        ]);

        let result = complete_with_correction(
            &backend, &make_request("list files"), 2, noop_callback, parse_command,
        );

        assert_eq!(result.unwrap(), "ls -la");
        assert_eq!(backend.call_count(), 1);
    }

    // -- Markdown fences corrected on retry --

    #[test]
    fn test_markdown_fences_corrected_on_retry() {
        let backend = MockBackend::new(vec![
            // First attempt: model wraps JSON in markdown fences
            Ok(CompletionResponse {
                content: "```json\n{\"command\":\"git revert HEAD\"}\n```".to_string(),
                is_truncated: false,
            }),
            // Second attempt: model responds with raw JSON
            Ok(CompletionResponse {
                content: r#"{"command":"git revert HEAD"}"#.to_string(),
                is_truncated: false,
            }),
        ]);

        let result = complete_with_correction(
            &backend, &make_request("undo last commit"), 2, noop_callback, parse_command,
        );

        assert_eq!(result.unwrap(), "git revert HEAD");
        assert_eq!(backend.call_count(), 2);
    }

    // -- Schema non-compliance corrected on retry --

    #[test]
    fn test_schema_violation_corrected_on_retry() {
        let backend = MockBackend::new(vec![
            // First attempt: model returns wrong schema (missing 'command' key)
            Ok(CompletionResponse {
                content: r#"{"suggestion":"rm -rf /"}"#.to_string(),
                is_truncated: false,
            }),
            // Second attempt: model fixes the schema
            Ok(CompletionResponse {
                content: r#"{"command":"rm -rf /"}"#.to_string(),
                is_truncated: false,
            }),
        ]);

        let result = complete_with_correction(
            &backend, &make_request("delete everything"), 2, noop_callback, parse_command,
        );

        assert_eq!(result.unwrap(), "rm -rf /");
        assert_eq!(backend.call_count(), 2);
    }

    // -- All retries exhausted --

    #[test]
    fn test_all_retries_exhausted() {
        let backend = MockBackend::new(vec![
            Ok(CompletionResponse { content: "bad".to_string(), is_truncated: false }),
            Ok(CompletionResponse { content: "still bad".to_string(), is_truncated: false }),
            Ok(CompletionResponse { content: "nope".to_string(), is_truncated: false }),
        ]);

        let result = complete_with_correction(
            &backend, &make_request("do something"), 2, noop_callback, parse_command,
        );

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, BackendError::ParseError(_)));
        assert!(err.to_string().contains("2 correction attempt(s)"));
        assert_eq!(backend.call_count(), 3); // 1 initial + 2 retries
    }

    // -- Zero retries means immediate failure --

    #[test]
    fn test_zero_retries_fails_immediately() {
        let backend = MockBackend::new(vec![
            Ok(CompletionResponse { content: "not json".to_string(), is_truncated: false }),
        ]);

        let result = complete_with_correction(
            &backend, &make_request("hello"), 0, noop_callback, parse_command,
        );

        assert!(matches!(result.unwrap_err(), BackendError::ParseError(_)));
        assert_eq!(backend.call_count(), 1);
    }

    // -- Backend errors propagate immediately, no retry --

    #[test]
    fn test_backend_error_propagates_immediately() {
        let backend = MockBackend::new(vec![
            Err(BackendError::RateLimited("slow down".to_string())),
        ]);

        let result = complete_with_correction(
            &backend, &make_request("hello"), 2, noop_callback, parse_command,
        );

        assert!(matches!(result.unwrap_err(), BackendError::RateLimited(_)));
        assert_eq!(backend.call_count(), 1);
    }

    // -- Message history stacks correctly across retries --

    #[test]
    fn test_history_stacks_across_retries() {
        let backend = MockBackend::new(vec![
            Ok(CompletionResponse { content: "bad1".to_string(), is_truncated: false }),
            Ok(CompletionResponse { content: "bad2".to_string(), is_truncated: false }),
            Ok(CompletionResponse {
                content: r#"{"command":"echo ok"}"#.to_string(),
                is_truncated: false,
            }),
        ]);

        let result = complete_with_correction(
            &backend, &make_request("say ok"), 2, noop_callback, parse_command,
        );
        assert_eq!(result.unwrap(), "echo ok");

        let requests = backend.requests();
        assert_eq!(requests.len(), 3);

        // First request: no history
        assert!(requests[0].message_history.is_empty());
        assert_eq!(requests[0].user_message, "say ok");

        // Second request: history has original user + failed assistant
        assert_eq!(requests[1].message_history.len(), 2);
        assert!(matches!(requests[1].message_history[0].role, MessageRole::User));
        assert_eq!(requests[1].message_history[0].content, "say ok");
        assert!(matches!(requests[1].message_history[1].role, MessageRole::Assistant));
        assert_eq!(requests[1].message_history[1].content, "bad1");
        assert!(requests[1].user_message.contains("could not be parsed"));

        // Third request: history has original + first correction + second failure
        assert_eq!(requests[2].message_history.len(), 4);
        assert!(matches!(requests[2].message_history[0].role, MessageRole::User));
        assert_eq!(requests[2].message_history[0].content, "say ok");
        assert!(matches!(requests[2].message_history[1].role, MessageRole::Assistant));
        assert_eq!(requests[2].message_history[1].content, "bad1");
        assert!(matches!(requests[2].message_history[2].role, MessageRole::User));
        assert!(requests[2].message_history[2].content.contains("could not be parsed"));
        assert!(matches!(requests[2].message_history[3].role, MessageRole::Assistant));
        assert_eq!(requests[2].message_history[3].content, "bad2");
    }

    // -- Callback factory receives correct attempt metadata --

    #[test]
    fn test_callback_receives_attempt_metadata() {
        let backend = MockBackend::new(vec![
            Ok(CompletionResponse { content: "bad".to_string(), is_truncated: false }),
            Ok(CompletionResponse {
                content: r#"{"command":"ok"}"#.to_string(),
                is_truncated: false,
            }),
        ]);

        let attempts_seen = Arc::new(Mutex::new(Vec::new()));
        let attempts_clone = attempts_seen.clone();

        let result = complete_with_correction(
            &backend,
            &make_request("test"),
            2,
            move |attempt| {
                attempts_clone.lock().unwrap().push((attempt.attempt, attempt.max_retries));
                Box::new(|_| StreamAction::Continue)
            },
            parse_command,
        );

        assert!(result.is_ok());
        let seen = attempts_seen.lock().unwrap();
        assert_eq!(*seen, vec![(0, 2), (1, 2)]);
    }

    // -- PrefixValidator unit tests --

    #[test]
    fn test_validator_accepts_valid_object_start() {
        let schema = serde_json::json!({"type": "object"});
        let mut v = PrefixValidator::from_schema(&schema).unwrap();
        assert_eq!(v.feed("{"), StreamAction::Continue);
        assert_eq!(v.feed("\"key\":1}"), StreamAction::Continue); // already decided
    }

    #[test]
    fn test_validator_accepts_leading_whitespace_then_brace() {
        let schema = serde_json::json!({"type": "object"});
        let mut v = PrefixValidator::from_schema(&schema).unwrap();
        assert_eq!(v.feed("  \n\t"), StreamAction::Continue); // all whitespace
        assert_eq!(v.feed("  {"), StreamAction::Continue);
    }

    #[test]
    fn test_validator_rejects_markdown_fences() {
        let schema = serde_json::json!({"type": "object"});
        let mut v = PrefixValidator::from_schema(&schema).unwrap();
        assert_eq!(v.feed("```json"), StreamAction::Abort);
    }

    #[test]
    fn test_validator_rejects_prose() {
        let schema = serde_json::json!({"type": "object"});
        let mut v = PrefixValidator::from_schema(&schema).unwrap();
        assert_eq!(v.feed("Here is the JSON:"), StreamAction::Abort);
    }

    #[test]
    fn test_validator_accepts_array_root() {
        let schema = serde_json::json!({"type": "array"});
        let mut v = PrefixValidator::from_schema(&schema).unwrap();
        assert_eq!(v.feed("["), StreamAction::Continue);
    }

    #[test]
    fn test_validator_rejects_array_when_object_expected() {
        let schema = serde_json::json!({"type": "object"});
        let mut v = PrefixValidator::from_schema(&schema).unwrap();
        assert_eq!(v.feed("["), StreamAction::Abort);
    }

    #[test]
    fn test_validator_none_for_string_schema() {
        let schema = serde_json::json!({"type": "string"});
        assert!(PrefixValidator::from_schema(&schema).is_none());
    }

    #[test]
    fn test_validator_none_for_no_type() {
        let schema = serde_json::json!({"properties": {}});
        assert!(PrefixValidator::from_schema(&schema).is_none());
    }

    // -- Streaming mock: verifies abort stops content from reaching the callback --

    /// A mock backend that feeds response content chunk-by-chunk through the callback,
    /// respecting abort signals.
    type StreamingResponse = (Vec<String>, Result<CompletionResponse, BackendError>);

    struct StreamingMockBackend {
        responses: Mutex<Vec<StreamingResponse>>,
        call_count: Mutex<usize>,
    }

    impl StreamingMockBackend {
        fn new(responses: Vec<(Vec<String>, Result<CompletionResponse, BackendError>)>) -> Self {
            Self {
                responses: Mutex::new(responses),
                call_count: Mutex::new(0),
            }
        }

        fn call_count(&self) -> usize {
            *self.call_count.lock().unwrap()
        }
    }

    impl Backend for StreamingMockBackend {
        fn complete_streaming(
            &self,
            _request: &CompletionRequest,
            callback: StreamCallback,
        ) -> Result<CompletionResponse, BackendError> {
            let mut count = self.call_count.lock().unwrap();
            let responses = self.responses.lock().unwrap();
            let idx = *count;
            *count += 1;

            let (chunks, result) = &responses[idx];
            let mut content = String::new();
            for chunk in chunks {
                content.push_str(chunk);
                if callback(StreamEvent::TextDelta(chunk.clone())) == StreamAction::Abort {
                    return Ok(CompletionResponse { content, is_truncated: false });
                }
            }
            result.clone()
        }
    }

    fn make_request_with_schema(user_message: &str) -> CompletionRequest {
        CompletionRequest {
            system_messages: vec!["You are helpful.".to_string()],
            message_history: vec![],
            user_message: user_message.to_string(),
            json_schema: Some(serde_json::json!({"type": "object"})),
            schema_name: "test".to_string(),
        }
    }

    #[test]
    fn test_abort_on_markdown_fences_skips_remaining_chunks() {
        let backend = StreamingMockBackend::new(vec![
            // First attempt: markdown fences — abort should fire on first chunk
            (
                vec!["```json\n".into(), r#"{"command":"ls"}"#.into(), "\n```".into()],
                Ok(CompletionResponse {
                    content: "```json\n{\"command\":\"ls\"}\n```".into(),
                    is_truncated: false,
                }),
            ),
            // Second attempt: clean JSON, streamed normally
            (
                vec![r#"{"command":"ls"}"#.into()],
                Ok(CompletionResponse {
                    content: r#"{"command":"ls"}"#.into(),
                    is_truncated: false,
                }),
            ),
        ]);

        // Track which chunks the inner callback actually sees
        let seen_chunks = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen = seen_chunks.clone();

        let result = complete_with_correction(
            &backend,
            &make_request_with_schema("list files"),
            2,
            move |_| {
                let seen = seen.clone();
                Box::new(move |event| {
                    if let StreamEvent::TextDelta(ref text) = event {
                        seen.lock().unwrap().push(text.clone());
                    }
                    StreamAction::Continue
                })
            },
            parse_command,
        );

        assert_eq!(result.unwrap(), "ls");
        assert_eq!(backend.call_count(), 2);

        // The inner callback sees the fenced chunk (so chars get counted)
        // plus the clean JSON from the second attempt
        let chunks = seen_chunks.lock().unwrap();
        assert_eq!(*chunks, vec!["```json\n", r#"{"command":"ls"}"#]);
    }

    #[test]
    fn test_valid_prefix_streams_normally() {
        let backend = StreamingMockBackend::new(vec![
            (
                vec!["{".into(), r#""command":"pwd"}"#.into()],
                Ok(CompletionResponse {
                    content: r#"{"command":"pwd"}"#.into(),
                    is_truncated: false,
                }),
            ),
        ]);

        let seen_chunks = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen = seen_chunks.clone();

        let result = complete_with_correction(
            &backend,
            &make_request_with_schema("show directory"),
            2,
            move |_| {
                let seen = seen.clone();
                Box::new(move |event| {
                    if let StreamEvent::TextDelta(ref text) = event {
                        seen.lock().unwrap().push(text.clone());
                    }
                    StreamAction::Continue
                })
            },
            parse_command,
        );

        assert_eq!(result.unwrap(), "pwd");
        assert_eq!(backend.call_count(), 1);

        // Both chunks should have been passed through
        let chunks = seen_chunks.lock().unwrap();
        assert_eq!(*chunks, vec!["{", r#""command":"pwd"}"#]);
    }

    #[test]
    fn test_no_schema_skips_validation() {
        let backend = StreamingMockBackend::new(vec![
            // No json_schema → validator disabled → fences reach the inner callback
            (
                vec!["```json\n".into(), r#"{"command":"ls"}"#.into()],
                Ok(CompletionResponse {
                    content: "```json\n{\"command\":\"ls\"}".into(),
                    is_truncated: false,
                }),
            ),
        ]);

        let seen_chunks = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen = seen_chunks.clone();

        // Use make_request (no schema) instead of make_request_with_schema
        let _ = complete_with_correction(
            &backend,
            &make_request("list files"),
            0,
            move |_| {
                let seen = seen.clone();
                Box::new(move |event| {
                    if let StreamEvent::TextDelta(ref text) = event {
                        seen.lock().unwrap().push(text.clone());
                    }
                    StreamAction::Continue
                })
            },
            parse_command,
        );

        // Without schema, fenced content passes through to the callback
        let chunks = seen_chunks.lock().unwrap();
        assert_eq!(*chunks, vec!["```json\n", r#"{"command":"ls"}"#]);
    }
}
