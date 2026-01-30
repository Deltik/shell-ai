//! Claude Code CLI backend (subprocess-based).

use super::{Backend, BackendError, CompletionRequest, CompletionResponse, StreamCallback, StreamEvent};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};

const MAX_ERRORS: usize = 3;

#[derive(Clone)]
pub struct ClaudeCodeBackend {
    cli_path: String,
    model: Option<String>,
}

impl ClaudeCodeBackend {
    pub fn new(cli_path: String, model: Option<String>) -> Self {
        Self { cli_path, model }
    }

    fn build_prompt(request: &CompletionRequest) -> String {
        let mut prompt = String::new();
        for sys_msg in &request.system_messages {
            prompt.push_str(sys_msg);
            prompt.push_str("\n\n");
        }
        prompt.push_str(&request.user_message);
        prompt
    }

    fn spawn_cli(&self, request: &CompletionRequest) -> Result<Child, BackendError> {
        let mut cmd = Command::new(&self.cli_path);
        cmd.arg("-p")
            .arg("--output-format").arg("stream-json")
            .arg("--include-partial-messages")
            .arg("--verbose")
            .arg("--debug-file").arg("/dev/null")
            .arg("--no-session-persistence")
            .arg("--tools").arg("")
            .arg("--system-prompt").arg("");

        if let Some(ref model) = self.model {
            cmd.arg("--model").arg(model);
        }
        if let Some(ref schema) = request.json_schema {
            let schema_str = serde_json::to_string(schema)
                .map_err(|e| BackendError::Other(format!("Failed to serialize JSON schema: {e}")))?;
            cmd.arg("--json-schema").arg(&schema_str);
        }

        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        log::debug!("Executing claude command: {:?}", cmd);

        cmd.spawn().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => BackendError::Other(format!(
                "Claude CLI not found at '{}'. Install from https://claude.ai/code or set cli_path.",
                self.cli_path
            )),
            _ => BackendError::NetworkError(format!("Failed to execute claude CLI: {e}")),
        })
    }

    fn handle_result(entry: &serde_json::Value) -> Result<Option<String>, BackendError> {
        if entry["subtype"].as_str() == Some("error") {
            let msg = entry["error"].as_str().unwrap_or("Unknown error");
            return Err(BackendError::ApiError(format!("Claude CLI error: {msg}")));
        }
        if let Some(structured) = entry.get("structured_output") {
            let content = serde_json::to_string(structured)
                .map_err(|e| BackendError::ParseError(format!("Failed to serialize structured_output: {e}")))?;
            return Ok(Some(content));
        }
        if let Some(result) = entry["result"].as_str() {
            return Ok(Some(result.to_string()));
        }
        Ok(None)
    }

    fn handle_stream_event(entry: &serde_json::Value, callback: &StreamCallback) {
        let event = &entry["event"];
        if event["type"].as_str() != Some("content_block_delta") {
            return;
        }
        let delta = &event["delta"];
        match delta["type"].as_str() {
            Some("text_delta") => {
                if let Some(text) = delta["text"].as_str() {
                    callback(StreamEvent::Preamble(text.to_string()));
                }
            }
            Some("input_json_delta") => {
                if let Some(json) = delta["partial_json"].as_str() {
                    callback(StreamEvent::TextDelta(json.to_string()));
                }
            }
            _ => {}
        }
    }

    /// Check partial assistant messages for a completed StructuredOutput tool use
    /// or API errors. Kills the child process early when structured output is found
    /// to avoid a wasted summary turn.
    fn handle_assistant(
        entry: &serde_json::Value,
        child: &mut Child,
        error_count: &mut usize,
        last_error: &mut Option<String>,
    ) -> Result<Option<String>, BackendError> {
        let Some(content) = entry["message"]["content"].as_array() else {
            return Ok(None);
        };

        for item in content {
            if item["type"].as_str() == Some("tool_use")
                && item["name"].as_str() == Some("StructuredOutput")
            {
                if let Some(input) = item.get("input") {
                    let json = serde_json::to_string(input)
                        .map_err(|e| BackendError::ParseError(format!("Failed to serialize tool input: {e}")))?;
                    let _ = child.kill();
                    return Ok(Some(json));
                }
            }
        }

        if let Some(error_type) = entry.get("error").and_then(|e| e.as_str()) {
            let error_text = content.iter().find_map(|item| item["text"].as_str());
            if let Some(text) = error_text {
                *last_error = Some(text.to_string());
            }
            *error_count += 1;
            log::debug!(
                "Claude CLI error [{error_type}] (count {error_count}): {:?}",
                last_error
            );
            if *error_count >= MAX_ERRORS {
                let _ = child.kill();
                let msg = last_error
                    .clone()
                    .unwrap_or_else(|| format!("Claude CLI error: {error_type}"));
                return Err(BackendError::ApiError(msg));
            }
        }

        Ok(None)
    }

    fn process_line(
        line: &str,
        child: &mut Child,
        error_count: &mut usize,
        last_error: &mut Option<String>,
        callback: &StreamCallback,
    ) -> Result<Option<String>, BackendError> {
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
            "result" => Self::handle_result(&entry),
            "stream_event" => {
                Self::handle_stream_event(&entry, callback);
                Ok(None)
            }
            "assistant" => Self::handle_assistant(&entry, child, error_count, last_error),
            _ => Ok(None),
        }
    }
}

impl Backend for ClaudeCodeBackend {
    fn complete_streaming(
        &self,
        request: &CompletionRequest,
        callback: StreamCallback,
    ) -> Result<CompletionResponse, BackendError> {
        let prompt = Self::build_prompt(request);
        let mut child = self.spawn_cli(request)?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(prompt.as_bytes())
                .map_err(|e| BackendError::NetworkError(format!("Failed to write to stdin: {e}")))?;
        }

        let stdout = child.stdout.take()
            .ok_or_else(|| BackendError::Other("Failed to capture stdout".to_string()))?;

        let reader = BufReader::new(stdout);
        let mut result_content: Option<String> = None;
        let mut last_error: Option<String> = None;
        let mut error_count = 0;

        for line_result in reader.lines() {
            let line = line_result
                .map_err(|e| BackendError::NetworkError(format!("Failed to read output: {e}")))?;

            if line.is_empty() {
                continue;
            }

            match Self::process_line(&line, &mut child, &mut error_count, &mut last_error, &callback) {
                Ok(Some(content)) => {
                    result_content = Some(content);
                    break;
                }
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
            Some(content) => Ok(CompletionResponse { content, is_truncated: false }),
            None => Err(if let Some(error) = last_error {
                BackendError::ApiError(error)
            } else if !status.success() {
                BackendError::ApiError(format!(
                    "Claude CLI exited with code {}", status.code().unwrap_or(-1)
                ))
            } else {
                BackendError::ParseError("No result found in output".to_string())
            }),
        }
    }
}