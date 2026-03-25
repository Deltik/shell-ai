use serde_json::Value;
use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};

use crate::config::FieldMeta;
use super::{HttpError, SseStream, STREAM_TIMEOUT_SECS};

/// Wraps a BufReader over ChildStdout and owns the Child handle.
/// When dropped, kills and reaps the child process.
struct ChildReader {
    reader: BufReader<std::process::ChildStdout>,
    child: Child,
}

impl Read for ChildReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.reader.read(buf)
    }
}

impl Drop for ChildReader {
    fn drop(&mut self) {
        // Kill the process (harmless if already exited) and reap it.
        // kill() is needed because the reader is dropped AFTER this runs,
        // so stdout is still open and wait() alone would block.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Send a POST request for SSE streaming using a curl-compatible subprocess.
///
/// `curl_cmd` is parsed using POSIX shell quoting rules (via `shlex`).
/// The first token is the binary; remaining tokens are prepended before
/// Shell-AI's own curl arguments.
pub fn post_json_streaming(
    curl_cmd: &str,
    meta: &FieldMeta,
    url: &str,
    bearer_token: Option<&str>,
    extra_headers: &[(&str, &str)],
    body: &Value,
) -> Result<SseStream, HttpError> {
    let body_bytes = serde_json::to_vec(body)
        .map_err(|e| HttpError::Network(format!("Failed to serialize request body: {}", e)))?;

    let setting_hint = meta.setting_hint();

    // Parse the command using POSIX shell quoting rules
    let cmd_parts = shlex::split(curl_cmd)
        .ok_or_else(|| HttpError::Config(format!(
            "{} has mismatched quotes: {}", meta.name, curl_cmd
        )))?;
    let (binary, user_args) = cmd_parts.split_first()
        .ok_or_else(|| HttpError::Config(format!(
            "{} is empty.\n{}", meta.name, setting_hint
        )))?;

    let mut args: Vec<String> = Vec::from(user_args);
    args.extend([
        "-s".into(),            // silent (no progress bar)
        "-S".into(),            // but show errors
        "-i".into(),            // include headers in output
        "--no-buffer".into(),   // disable output buffering for real-time SSE
        "-X".into(),
        "POST".into(),
        "--max-time".into(),
        STREAM_TIMEOUT_SECS.to_string(),
        "-H".into(),
        "Content-Type: application/json".into(),
        "-H".into(),
        "Accept: text/event-stream".into(),
    ]);

    if let Some(token) = bearer_token {
        args.push("-H".into());
        args.push(format!("Authorization: Bearer {}", token));
    }

    for (k, v) in extra_headers {
        args.push("-H".into());
        args.push(format!("{}: {}", k, v));
    }

    // Read body from stdin to avoid shell escaping issues with large JSON
    args.push("-d".into());
    args.push("@-".into());
    args.push(url.into());

    let mut child = Command::new(binary)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| HttpError::Config(format!(
            "{} command '{}' could not be started: {}\n{}", meta.name, binary, e, setting_hint
        )))?;

    // Write body to stdin, then close it
    {
        use std::io::Write;
        let mut stdin = child.stdin.take()
            .ok_or_else(|| HttpError::Network("Failed to open stdin for curl subprocess".into()))?;
        stdin.write_all(&body_bytes)
            .map_err(|e| HttpError::Network(format!("Failed to write body to curl stdin: {}", e)))?;
        // stdin dropped here → curl sees EOF and starts the request
    }

    // Read headers from stdout (curl -i outputs headers before body)
    let stdout = child.stdout.take()
        .ok_or_else(|| HttpError::Network("Failed to open stdout from curl subprocess".into()))?;
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();

    // Parse status line: "HTTP/2 200" or "HTTP/1.1 200 OK"
    let bytes_read = reader.read_line(&mut line)
        .map_err(|e| HttpError::Network(format!("Failed to read curl output: {}", e)))?;
    if bytes_read == 0 {
        // No output — curl failed. Read stderr for the error message.
        let err_msg = read_stderr(&mut child);
        return Err(HttpError::Network(if err_msg.is_empty() {
            format!("'{}' produced no output", curl_cmd)
        } else {
            err_msg
        }));
    }

    let status: u16 = line.split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    if status == 0 {
        let err_msg = read_stderr(&mut child);
        return Err(HttpError::Network(if err_msg.is_empty() {
            format!("'{}' returned unparseable response: {}", curl_cmd, line.trim())
        } else {
            err_msg
        }));
    }

    // Parse remaining headers until blank line
    let mut retry_after_secs: Option<u64> = None;
    loop {
        line.clear();
        let n = reader.read_line(&mut line)
            .map_err(|e| HttpError::Network(format!("Failed to read headers: {}", e)))?;
        if n == 0 {
            break; // EOF before blank line — unusual but handle it
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break; // End of headers
        }
        let lower = trimmed.to_lowercase();
        if let Some(value) = lower.strip_prefix("retry-after:") {
            retry_after_secs = value.trim().parse().ok();
        }
    }

    // The remaining reader is the SSE body stream.
    // ChildReader owns both the reader (with buffered data) and the child process.
    let child_reader = ChildReader { reader, child };
    let boxed: Box<dyn Read + Send> = Box::new(child_reader);

    Ok(SseStream {
        reader: BufReader::new(boxed),
        status,
        retry_after_secs,
    })
}

/// Read stderr from the child process for error reporting.
fn read_stderr(child: &mut Child) -> String {
    child.stderr.take()
        .and_then(|mut stderr| {
            let mut buf = String::new();
            stderr.read_to_string(&mut buf).ok()?;
            Some(buf.trim().to_string())
        })
        .unwrap_or_default()
}