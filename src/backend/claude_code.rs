//! Claude Code CLI backend (subprocess-based).
//!
//! This backend uses the Claude Code CLI (`claude`) in non-interactive mode
//! with structured output via `--json-schema`.

use super::{Backend, BackendError, CompletionRequest, CompletionResponse};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

/// Backend for Claude Code CLI (subprocess execution).
#[derive(Clone)]
pub struct ClaudeCodeBackend {
    /// Path to the claude CLI executable
    cli_path: String,
    /// Optional model override
    model: Option<String>,
}

impl ClaudeCodeBackend {
    /// Create a new Claude Code backend.
    pub fn new(cli_path: String, model: Option<String>) -> Self {
        Self { cli_path, model }
    }
}

impl Backend for ClaudeCodeBackend {
    fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse, BackendError> {
        // Build the prompt combining system and user messages
        let mut prompt = String::new();

        // Add system messages
        for sys_msg in &request.system_messages {
            prompt.push_str(sys_msg);
            prompt.push_str("\n\n");
        }

        // Add user message
        prompt.push_str(&request.user_message);

        // Build command
        let mut cmd = Command::new(&self.cli_path);
        cmd.arg("-p")
            .arg("--output-format")
            .arg("stream-json")
            .arg("--include-partial-messages")
            .arg("--debug-file")
            .arg("/dev/null")
            .arg("--no-session-persistence")
            .arg("--tools")
            .arg("")
            .arg("--system-prompt")
            .arg("");

        // Add model override if specified
        if let Some(ref model) = self.model {
            cmd.arg("--model").arg(model);
        }

        // Add JSON schema constraint for structured output
        if let Some(ref schema) = request.json_schema {
            let schema_str = serde_json::to_string(schema)
                .map_err(|e| BackendError::Other(anyhow::anyhow!("Failed to serialize JSON schema: {}", e)))?;
            cmd.arg("--json-schema").arg(&schema_str);
        }

        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        log::debug!("Executing claude command: {:?}", cmd);

        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                BackendError::Other(anyhow::anyhow!(
                    "Claude CLI not found at '{}'. Install it from https://claude.ai/code or set cli_path in config.",
                    self.cli_path
                ))
            } else {
                BackendError::NetworkError(format!("Failed to execute claude CLI: {}", e))
            }
        })?;

        // Write prompt to stdin and close it
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(prompt.as_bytes()).map_err(|e| {
                BackendError::NetworkError(format!("Failed to write to claude CLI stdin: {}", e))
            })?;
        }

        // Read stdout line by line (stream-json format)
        let stdout = child.stdout.take().ok_or_else(|| {
            BackendError::Other(anyhow::anyhow!("Failed to capture claude CLI stdout"))
        })?;

        let reader = BufReader::new(stdout);
        let mut result_content: Option<String> = None;
        let mut last_error: Option<String> = None;
        let mut error_count = 0;
        const MAX_ERRORS: usize = 3;

        for line_result in reader.lines() {
            let line = line_result.map_err(|e| {
                BackendError::NetworkError(format!("Failed to read claude CLI output: {}", e))
            })?;

            if line.is_empty() {
                continue;
            }

            let entry: serde_json::Value = serde_json::from_str(&line).map_err(|e| {
                BackendError::ParseError(format!(
                    "Failed to parse Claude CLI JSON line: {}\nLine: {}",
                    e,
                    if line.len() > 200 {
                        format!("{}...", &line[..200])
                    } else {
                        line.clone()
                    }
                ))
            })?;

            let msg_type = entry.get("type").and_then(|t| t.as_str()).unwrap_or("");

            match msg_type {
                "result" => {
                    // Check for success or error
                    let subtype = entry.get("subtype").and_then(|s| s.as_str()).unwrap_or("");
                    if subtype == "error" {
                        let error_msg = entry
                            .get("error")
                            .and_then(|e| e.as_str())
                            .unwrap_or("Unknown error");
                        return Err(BackendError::ApiError(format!(
                            "Claude CLI error: {}",
                            error_msg
                        )));
                    }

                    // Extract structured_output or result
                    if let Some(structured) = entry.get("structured_output") {
                        result_content = Some(serde_json::to_string(structured).map_err(|e| {
                            BackendError::ParseError(format!(
                                "Failed to serialize structured_output: {}",
                                e
                            ))
                        })?);
                    } else if let Some(result) = entry.get("result").and_then(|r| r.as_str()) {
                        result_content = Some(result.to_string());
                    }
                    break;
                }
                "assistant" => {
                    // Check for API errors (e.g., invalid model)
                    if entry.get("error").is_some() {
                        // Extract error from content
                        if let Some(content) = entry.get("message").and_then(|m| m.get("content")) {
                            if let Some(arr) = content.as_array() {
                                for item in arr {
                                    if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                                        if text.contains("API Error:") {
                                            last_error = Some(text.to_string());
                                            error_count += 1;
                                            log::debug!(
                                                "Claude CLI API error (count {}): {}",
                                                error_count,
                                                text
                                            );
                                            if error_count >= MAX_ERRORS {
                                                // Kill the child process
                                                let _ = child.kill();
                                                return Err(BackendError::ApiError(text.to_string()));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                "system" | "stream_event" | "user" => {
                    // Informational messages, continue processing
                }
                _ => {
                    log::debug!("Unknown Claude CLI message type: {}", msg_type);
                }
            }
        }

        // Wait for child to exit
        let status = child.wait().map_err(|e| {
            BackendError::NetworkError(format!("Failed to wait for claude CLI: {}", e))
        })?;

        // If we had errors but no result, report the last error
        if result_content.is_none() {
            if let Some(error) = last_error {
                return Err(BackendError::ApiError(error));
            }
            if !status.success() {
                return Err(BackendError::ApiError(format!(
                    "Claude CLI failed with exit code {}",
                    status.code().unwrap_or(-1)
                )));
            }
            return Err(BackendError::ParseError(
                "No result found in Claude CLI output".to_string(),
            ));
        }

        let is_truncated = false;

        Ok(CompletionResponse {
            content: result_content.unwrap(),
            is_truncated,
        })
    }
}