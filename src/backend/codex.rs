//! OpenAI Codex CLI backend (subprocess-based).

use super::{Backend, BackendError, CompletionRequest, CompletionResponse, MessageRole, StreamCallback, StreamEvent};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone)]
pub struct CodexBackend {
    cli_path: String,
    model: Option<String>,
    effort: Option<String>,
}

/// RAII guard that deletes a temporary schema file when dropped.
struct TempSchemaFile {
    path: PathBuf,
}

impl Drop for TempSchemaFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

impl CodexBackend {
    pub fn new(cli_path: String, model: Option<String>, effort: Option<String>) -> Self {
        Self { cli_path, model, effort }
    }

    fn build_prompt(request: &CompletionRequest) -> String {
        let mut prompt = String::new();
        for sys_msg in &request.system_messages {
            prompt.push_str(sys_msg);
            prompt.push_str("\n\n");
        }

        for msg in &request.message_history {
            let label = match msg.role {
                MessageRole::User => "User",
                MessageRole::Assistant => "Assistant",
            };
            prompt.push_str(&format!("{}: {}\n\n", label, msg.content));
        }

        prompt.push_str(&request.user_message);
        prompt
    }

    /// Write the request schema to a temp file and return a guard that deletes
    /// it on drop. Returns `None` if the request has no schema.
    fn write_schema_temp_file(request: &CompletionRequest) -> Result<Option<TempSchemaFile>, BackendError> {
        let Some(schema) = &request.json_schema else {
            return Ok(None);
        };
        let body = serde_json::to_string(schema)
            .map_err(|e| BackendError::Other(format!("Failed to serialize JSON schema: {e}")))?;
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let pid = std::process::id();
        // create_new (O_EXCL) so a stale or foreign file at the same path is an
        // error rather than silently reused; collide → try the next counter.
        for _ in 0..16 {
            let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("shai-codex-schema-{pid}-{counter}.json"));
            match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    let guard = TempSchemaFile { path };
                    file.write_all(body.as_bytes())
                        .map_err(|e| BackendError::Other(format!("Failed to write schema temp file: {e}")))?;
                    return Ok(Some(guard));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(BackendError::Other(format!("Failed to create schema temp file: {e}"))),
            }
        }
        Err(BackendError::Other(
            "Failed to create schema temp file: too many name collisions in temp dir".to_string(),
        ))
    }

    fn spawn_cli(&self, schema_file: Option<&TempSchemaFile>) -> Result<Child, BackendError> {
        let tokens = shlex::split(&self.cli_path).ok_or_else(|| {
            BackendError::ConfigError(format!(
                "Invalid cli_path '{}': mismatched quotes or shell metacharacters",
                self.cli_path
            ))
        })?;
        let mut iter = tokens.into_iter();
        let program = iter.next().ok_or_else(|| {
            BackendError::ConfigError("cli_path is empty".to_string())
        })?;

        let mut cmd = Command::new(&program);
        for arg in iter {
            cmd.arg(arg);
        }

        cmd.arg("exec")
            .arg("--json")
            .arg("--color").arg("never")
            .arg("--skip-git-repo-check")
            .arg("--ephemeral")
            .arg("--sandbox").arg("read-only")
            .arg("--disable").arg("shell_tool")
            .arg("--disable").arg("browser_use")
            .arg("--disable").arg("computer_use")
            .arg("--disable").arg("apps");

        if let Some(ref model) = self.model {
            cmd.arg("-m").arg(model);
        }
        if let Some(ref effort) = self.effort {
            // Codex parses the value as TOML, falling back to a literal string
            cmd.arg("-c").arg(format!("model_reasoning_effort={effort}"));
        }
        if let Some(file) = schema_file {
            cmd.arg("--output-schema").arg(&file.path);
        }

        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        log::debug!("Executing codex command: {:?}", cmd);

        cmd.spawn().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => BackendError::ConfigError(format!(
                "Codex CLI not found at '{}'. Install from https://github.com/openai/codex \
                or set cli_path (e.g., \"npx @openai/codex@latest\").",
                program
            )),
            _ => BackendError::NetworkError(format!("Failed to execute codex CLI: {e}")),
        })
    }

    /// Extract a human-readable error message from a Codex `error` or
    /// `turn.failed` event.  Codex wraps the upstream OpenAI error JSON in
    /// the `message` field as a string; if that string parses as JSON, prefer
    /// its inner `error.message`, else use the raw string.
    fn extract_error_message(entry: &serde_json::Value) -> String {
        let raw = entry["message"]
            .as_str()
            .or_else(|| entry["error"]["message"].as_str())
            .unwrap_or("Codex CLI error");
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(raw) {
            if let Some(inner) = parsed["error"]["message"].as_str() {
                return inner.to_string();
            }
        }
        raw.to_string()
    }

    /// Returns `Ok(Some(text))` if this line carried the final agent message,
    /// `Ok(None)` to keep reading, or `Err` on a Codex-side error.
    fn process_line(line: &str) -> Result<Option<String>, BackendError> {
        let entry: serde_json::Value = serde_json::from_str(line).map_err(|e| {
            let preview = if line.len() > 200 {
                let end = (0..=200).rev().find(|&i| line.is_char_boundary(i)).unwrap_or(0);
                &line[..end]
            } else {
                line
            };
            BackendError::ParseError(format!("Failed to parse JSON: {e}\nLine: {preview}"))
        })?;

        match entry["type"].as_str().unwrap_or("") {
            "item.completed" => {
                let item = &entry["item"];
                if item["type"].as_str() == Some("agent_message") {
                    if let Some(text) = item["text"].as_str() {
                        return Ok(Some(text.to_string()));
                    }
                }
                Ok(None)
            }
            "error" | "turn.failed" => {
                Err(BackendError::ApiError(Self::extract_error_message(&entry)))
            }
            _ => Ok(None),
        }
    }
}

impl Backend for CodexBackend {
    fn complete_streaming(
        &self,
        request: &CompletionRequest,
        callback: StreamCallback,
    ) -> Result<CompletionResponse, BackendError> {
        let prompt = Self::build_prompt(request);
        let schema_file = Self::write_schema_temp_file(request)?;
        let mut child = self.spawn_cli(schema_file.as_ref())?;

        // Drain stderr on a background thread so a chatty child can't fill the
        // pipe buffer and deadlock against our stdout reads. The thread ends at
        // EOF when the child exits.
        let stderr_thread = child.stderr.take().map(|mut stderr| {
            std::thread::spawn(move || {
                let mut buf = String::new();
                use std::io::Read;
                let _ = stderr.read_to_string(&mut buf);
                buf
            })
        });
        let read_stderr = move || -> Option<String> {
            stderr_thread
                .and_then(|t| t.join().ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        };

        if let Some(mut stdin) = child.stdin.take() {
            if let Err(e) = stdin.write_all(prompt.as_bytes()) {
                let _ = child.kill();
                let _ = child.wait();
                // A child that dies before reading the prompt (bad flags, auth
                // failure) surfaces as a broken pipe; its stderr explains why.
                return Err(match read_stderr() {
                    Some(msg) => BackendError::ApiError(msg),
                    None => BackendError::NetworkError(format!("Failed to write to stdin: {e}")),
                });
            }
        }

        let stdout = child.stdout.take()
            .ok_or_else(|| BackendError::Other("Failed to capture stdout".to_string()))?;

        let reader = BufReader::new(stdout);
        let mut result_content: Option<String> = None;

        // Read to EOF rather than stopping at the first agent message: this
        // drains the pipe (no deadlock on trailing events) and makes the last
        // agent message win, matching codex's own --output-last-message rule.
        for line_result in reader.lines() {
            let line = match line_result {
                Ok(line) => line,
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(BackendError::NetworkError(format!("Failed to read output: {e}")));
                }
            };

            if line.is_empty() {
                continue;
            }

            match Self::process_line(&line) {
                Ok(Some(content)) => result_content = Some(content),
                Ok(None) => {}
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(e);
                }
            }
        }

        let status = child.wait()
            .map_err(|e| BackendError::NetworkError(format!("Failed to wait for process: {e}")))?;

        match result_content {
            Some(content) => {
                // The child has already exited by the time this single delta is
                // emitted, so there is nothing left to abort and the callback's
                // StreamAction is safely ignorable.
                callback(StreamEvent::TextDelta(content.clone()));
                Ok(CompletionResponse { content, is_truncated: false })
            }
            None => {
                let detail = read_stderr().unwrap_or_else(|| {
                    format!("Codex CLI exited with code {}", status.code().unwrap_or(-1))
                });
                Err(BackendError::ApiError(detail))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::HistoryMessage;

    fn request_with_history() -> CompletionRequest {
        CompletionRequest {
            system_messages: vec!["sys1".to_string(), "sys2".to_string()],
            message_history: vec![
                HistoryMessage { role: MessageRole::User, content: "prior question".to_string() },
                HistoryMessage { role: MessageRole::Assistant, content: "prior answer".to_string() },
            ],
            user_message: "final question".to_string(),
            json_schema: None,
            schema_name: "test".to_string(),
        }
    }

    #[test]
    fn build_prompt_layout() {
        let prompt = CodexBackend::build_prompt(&request_with_history());
        assert_eq!(
            prompt,
            "sys1\n\nsys2\n\nUser: prior question\n\nAssistant: prior answer\n\nfinal question"
        );
    }

    #[test]
    fn process_line_returns_agent_message_text() {
        let line = r#"{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"{\"cmd\":\"ls\"}"}}"#;
        assert_eq!(
            CodexBackend::process_line(line).unwrap(),
            Some("{\"cmd\":\"ls\"}".to_string())
        );
    }

    #[test]
    fn process_line_ignores_other_events() {
        for line in [
            r#"{"type":"thread.started","thread_id":"t1"}"#,
            r#"{"type":"turn.started"}"#,
            r#"{"type":"item.completed","item":{"type":"reasoning","text":"hmm"}}"#,
            r#"{"type":"turn.completed","usage":{"input_tokens":9492}}"#,
        ] {
            assert_eq!(CodexBackend::process_line(line).unwrap(), None, "line: {line}");
        }
    }

    #[test]
    fn process_line_unwraps_nested_api_error() {
        // Codex wraps the upstream OpenAI error JSON as a string in `message`.
        let line = r#"{"type":"error","message":"{\n  \"type\": \"error\",\n  \"error\": {\n    \"type\": \"invalid_request_error\",\n    \"message\": \"Invalid value: 'bogus'\"\n  },\n  \"status\": 400\n}"}"#;
        match CodexBackend::process_line(line) {
            Err(BackendError::ApiError(msg)) => assert_eq!(msg, "Invalid value: 'bogus'"),
            other => panic!("expected ApiError, got {:?}", other),
        }
    }

    #[test]
    fn process_line_turn_failed_plain_message() {
        let line = r#"{"type":"turn.failed","error":{"message":"something broke"}}"#;
        match CodexBackend::process_line(line) {
            Err(BackendError::ApiError(msg)) => assert_eq!(msg, "something broke"),
            other => panic!("expected ApiError, got {:?}", other),
        }
    }

    #[test]
    fn process_line_rejects_malformed_json() {
        match CodexBackend::process_line("not json") {
            Err(BackendError::ParseError(_)) => {}
            other => panic!("expected ParseError, got {:?}", other),
        }
    }
}