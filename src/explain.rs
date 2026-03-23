use anyhow::{anyhow, bail, Result};
use colored::Colorize;
use is_terminal::IsTerminal;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::backend::{Backend, BackendError, CompletionRequest, CompletionResponse, StreamAction, StreamEvent};
use crate::backend::correction::complete_with_correction;
use crate::config::{resolve_locale, OutputFormat, ValidatedConfig};
use crate::preview::StreamingPreview;
use crate::progress::Progress;

/// A man page reference with metadata for sorting.
#[derive(Debug, Clone)]
struct ManReference {
    command: String,
    content: String,
    char_count: usize,
}

/// Extract potential command names from shell syntax.
/// Splits on shell operators and takes the first word of each segment.
fn extract_command_names(shell_cmd: &str) -> Vec<String> {
    let mut commands = Vec::new();

    // Split on common shell operators: | && || ; ( ) $( `
    let separators = ['|', '&', ';', '(', ')', '`', '\n'];

    for segment in shell_cmd.split(|c| separators.contains(&c)) {
        let segment = segment.trim();
        if segment.is_empty() {
            continue;
        }

        // Skip if starts with $ (likely variable or subshell remnant)
        if segment.starts_with('$') {
            continue;
        }

        // Split into words
        let words: Vec<&str> = segment.split_whitespace().collect();

        for word in words {
            // Skip env var assignments (VAR=value)
            if word.contains('=') && !word.starts_with('-') {
                continue;
            }
            // Skip redirections
            if word.starts_with('<') || word.starts_with('>') {
                continue;
            }
            // Skip numbers (like in `2>&1`)
            if word.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }

            // This might be a command - strip any leading ./
            let cmd = word.trim_start_matches("./");
            if !cmd.is_empty() && !cmd.starts_with('-') {
                commands.push(cmd.to_string());
                break; // Only first command-like word per segment
            }
        }
    }

    // Deduplicate while preserving order
    let mut seen = std::collections::HashSet::new();
    commands.retain(|c| seen.insert(c.clone()));

    commands
}

/// Check if a man page exists for a command using `man -w`.
fn has_man_page(cmd: &str) -> bool {
    Command::new("man")
        .args(["-w", cmd])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or_else(|e| {
            log::debug!("Failed to check man page for '{}': {}", cmd, e);
            false
        })
}

/// Fetch man page for a command, extracting primarily the OPTIONS section.
/// Returns None if the command has no man page or fetching fails.
fn get_man_page(cmd: &str, max_chars: usize) -> Option<String> {
    // First check if man page exists
    if !has_man_page(cmd) {
        return None;
    }

    // Fetch the man page with wide width to reduce line breaks (saves tokens)
    let output = match Command::new("man")
        .arg(cmd)
        .env("MANWIDTH", "100000")
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            log::debug!("Failed to run man command for '{}': {}", cmd, e);
            return None;
        }
    };

    if !output.status.success() {
        return None;
    }

    let raw = String::from_utf8_lossy(&output.stdout);

    // Try to extract just the OPTIONS section, with fallback
    let content = extract_options_section(&raw).unwrap_or_else(|| {
        // If no OPTIONS section, take the beginning of the man page
        truncate_to_limit(&raw, max_chars)
    });

    // Cap individual man page size
    let capped = truncate_to_limit(&content, max_chars);

    if capped.is_empty() {
        None
    } else {
        Some(format!("# {}(1)\n\n{}", cmd, capped))
    }
}

/// Extract the OPTIONS section from a man page, falling back to DESCRIPTION.
fn extract_options_section(man_page: &str) -> Option<String> {
    // Try OPTIONS first, then fall back to DESCRIPTION
    extract_section(man_page, "OPTIONS")
        .or_else(|| extract_section(man_page, "DESCRIPTION"))
}

/// Extract a specific section from a man page by header name.
fn extract_section(man_page: &str, section_name: &str) -> Option<String> {
    let lines: Vec<&str> = man_page.lines().collect();
    let mut result = Vec::new();
    let mut in_section = false;

    for line in lines {
        let trimmed = line.trim();

        // Detect section headers (typically ALL CAPS at start of line, no leading whitespace)
        let is_header = !line.starts_with(' ') && !line.starts_with('\t')
            && !trimmed.is_empty()
            && trimmed.chars().next().map(|c| c.is_ascii_uppercase()).unwrap_or(false);

        if is_header {
            if trimmed.starts_with(section_name) {
                in_section = true;
                result.push(line);
            } else if in_section {
                // We've hit another section, stop
                break;
            }
        } else if in_section {
            result.push(line);
        }
    }

    if !result.is_empty() {
        Some(result.join("\n"))
    } else {
        None
    }
}

/// Truncate text to a maximum character limit at a line boundary.
fn truncate_to_limit(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }

    // Find the nearest char boundary at or before max_chars
    let boundary = (0..=max_chars)
        .rev()
        .find(|&i| text.is_char_boundary(i))
        .unwrap_or(0);
    let truncated = &text[..boundary];
    if let Some(last_newline) = truncated.rfind('\n') {
        format!("{}...\n[truncated]", &text[..last_newline])
    } else {
        format!("{}...\n[truncated]", truncated)
    }
}

/// Gather man page references for commands in a shell command string.
fn gather_man_references(shell_cmd: &str, max_total_chars: u32) -> Vec<ManReference> {
    let commands = extract_command_names(shell_cmd);
    let max_per_page = (max_total_chars as usize) / 2; // Cap each page at half of total

    let mut references: Vec<ManReference> = commands
        .iter()
        .filter_map(|cmd| {
            get_man_page(cmd, max_per_page).map(|content| ManReference {
                command: cmd.clone(),
                char_count: content.len(),
                content,
            })
        })
        .collect();

    // Sort by size ascending (shortest first = dropped first when over limit)
    references.sort_by_key(|r| r.char_count);

    // Pre-filter to stay under max_total_chars
    let mut total_chars = 0;
    references.retain(|r| {
        if total_chars + r.char_count <= max_total_chars as usize {
            total_chars += r.char_count;
            true
        } else {
            false
        }
    });

    references
}

#[derive(Debug, Deserialize, Serialize)]
struct ExplanationNode {
    segment: String,
    #[serde(default)]
    citation: Option<String>,
    #[serde(default)]
    citation_confidence: Option<f32>,
    prefix: Option<String>,
    #[serde(default)]
    suffix: Option<String>,
    children: Vec<ExplanationNode>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ExplainResult {
    explanations: Vec<ExplanationNode>,
    synopsis: String,
}

/// Build the JSON schema for explain output.
/// When `with_citations` is true, includes citation and citation_confidence fields.
fn build_explain_schema(with_citations: bool) -> serde_json::Value {
    use serde_json::json;

    let mut properties = serde_json::Map::new();
    let mut required = vec!["segment", "prefix", "suffix", "children"];

    properties.insert("segment".to_string(), json!({
        "type": "string",
        "description": "The exact token from the command (direct quote, will be highlighted)"
    }));

    if with_citations {
        properties.insert("citation".to_string(), json!({
            "type": ["string", "null"],
            "description": "A verbatim quote from the provided documentation that describes this segment. Leave null if no documentation was provided or the segment is not documented."
        }));
        properties.insert("citation_confidence".to_string(), json!({
            "type": "number",
            "minimum": 0.0,
            "maximum": 1.0,
            "description": "Confidence score (0.0 to 1.0) for the citation accuracy. 1 = exact quote from docs, 0 = no docs available or pure guess."
        }));
        required.push("citation");
        required.push("citation_confidence");
    }

    properties.insert("prefix".to_string(), json!({
        "type": ["string", "null"],
        "description": "Optional text before the segment that forms the start of a sentence"
    }));
    properties.insert("suffix".to_string(), json!({
        "type": ["string", "null"],
        "description": "Text after the segment that completes the sentence"
    }));
    properties.insert("children".to_string(), json!({
        "type": "array",
        "items": { "$ref": "#/$defs/explanation" },
        "description": "Nested explanations for sub-components"
    }));

    json!({
        "type": "object",
        "properties": {
            "explanations": {
                "type": "array",
                "items": { "$ref": "#/$defs/explanation" }
            },
            "synopsis": {
                "type": "string",
                "description": "A one-line description of what the overall command does"
            }
        },
        "required": ["explanations", "synopsis"],
        "additionalProperties": false,
        "$defs": {
            "explanation": {
                "type": "object",
                "properties": properties,
                "required": required,
                "additionalProperties": false
            }
        }
    })
}

/// Build the system prompt for the explain command.
/// When `with_citations` is true, includes citation instructions.
/// When `locale` is Some, includes a hint to respond in that language.
fn build_system_prompt(with_citations: bool, locale: Option<&str>) -> String {
    let mut prompt = String::from(
        "You are a shell command explainer. The user will provide a shell command, \
         and you will explain it by breaking it down into its components.\n\n"
    );

    if let Some(loc) = locale {
        prompt.push_str(&format!(
            "Respond in the user's preferred locale/language: {}\n\n",
            loc
        ));
    }

    if with_citations {
        prompt.push_str(
            "For each segment, you MUST:\n\
             1. First identify the exact segment from the command\n\
             2. Look up the segment in the provided documentation and quote it VERBATIM in \"citation\"\n\
             3. Rate your citation confidence (1.0 = exact quote from docs, 0.0 = no docs or guessing)\n\
             4. Then write the explanation (prefix + segment + suffix forms a natural sentence)\n\n"
        );
    }

    prompt.push_str("Output format: JSON with \"explanations\" array, then \"synopsis\" string.\n\n");
    prompt.push_str("Each explanation node has these fields:\n");
    prompt.push_str("- \"segment\": The exact token from the command (will be highlighted)\n");

    if with_citations {
        prompt.push_str("- \"citation\": Verbatim quote from provided documentation, or null if unavailable\n");
        prompt.push_str("- \"citation_confidence\": 0.0-1.0 confidence in citation accuracy\n");
    }

    prompt.push_str("- \"prefix\": Optional text before segment (start of sentence)\n");
    prompt.push_str("- \"suffix\": Text after segment (completes the sentence)\n");
    prompt.push_str("- \"children\": Nested explanations for sub-components (combined flags or control flow)\n\n");

    prompt.push_str("The rendered output is: \"{prefix} {segment} {suffix}\" - this MUST be a natural sentence.\n\n");

    prompt.push_str("IMPORTANT: \"segment\" must be EXACT characters from the command, no escaping changes.\n");
    if with_citations {
        prompt.push_str("\"citation\" must be VERBATIM from the documentation, i.e., copy-paste, don't paraphrase.\n");
        prompt.push_str("Base your explanation on the citation, not prior knowledge.\n");
    }

    prompt.push_str("\nExample:\n");
    if with_citations {
        prompt.push_str(
            "{\n  \"segment\": \"-x\",\n  \
             \"citation\": \"-x, --example  Description of what this option does.\",\n  \
             \"citation_confidence\": 1.0,\n  \
             \"prefix\": null,\n  \
             \"suffix\": \"does something specific.\",\n  \
             \"children\": []\n}\n\n"
        );
    } else {
        prompt.push_str(
            "{\n  \"segment\": \"-x\",\n  \
             \"prefix\": null,\n  \
             \"suffix\": \"does something specific.\",\n  \
             \"children\": []\n}\n\n"
        );
    }

    prompt.push_str("Rules:\n");
    prompt.push_str("1. \"segment\" MUST be an exact substring from the command\n");
    prompt.push_str("2. \"{prefix} {segment} {suffix}\" must read as a complete sentence\n");
    prompt.push_str("3. Use \"children\" to break down combined flags (e.g., \"-abc\" into \"-a\", \"-b\", \"-c\") ");
    prompt.push_str("or complex control flow (e.g., loops, conditionals, pipelines)\n");
    prompt.push_str("4. Keep explanations concise\n");
    if with_citations {
        prompt.push_str("5. USE the provided documentation - cite verbatim and base explanation on it\n");
    }

    prompt
}

#[derive(Debug)]
pub struct ExplainOptions {
    pub command: Vec<String>,
}

pub async fn run_explain(validated: &ValidatedConfig<'_>, opts: ExplainOptions) -> Result<()> {
    // Determine command input: from args, or from stdin when piped.
    let mut command_to_explain = if !opts.command.is_empty() {
        opts.command.join(" ")
    } else {
        let mut buf = String::new();
        if std::io::stdin().is_terminal() {
            buf
        } else {
            std::io::stdin().read_to_string(&mut buf)?;
            buf
        }
    };
    command_to_explain = command_to_explain.trim().to_string();
    if command_to_explain.is_empty() {
        bail!("Command to explain is empty");
    }

    explain_command(&command_to_explain, validated).await
}

/// Explain a command directly (callable from other modules)
pub async fn explain_command(command_to_explain: &str, validated: &ValidatedConfig<'_>) -> Result<()> {
    let config = validated.app_config();
    let command_to_explain = command_to_explain.trim();
    if command_to_explain.is_empty() {
        bail!("Command to explain is empty");
    }

    // Create backend for API calls (Arc for sharing across threads)
    let backend: Arc<dyn Backend> = Arc::from(validated.create_backend());

    // Create progress indicator for gathering docs phase
    let progress = Progress::new("Gathering documentation...");

    // Gather man page references for context
    let mut references = if config.max_reference_chars.value > 0 {
        gather_man_references(command_to_explain, config.max_reference_chars.value)
    } else {
        Vec::new()
    };

    // Clear the initial progress
    if let Some(ref p) = progress {
        p.finish_and_clear();
    }
    drop(progress);

    log::debug!("Extracted commands: {:?}", extract_command_names(command_to_explain));
    log::debug!("Man page references gathered: {}", references.len());
    for r in &references {
        log::debug!("  - {} ({} chars)", r.command, r.char_count);
    }

    // Resolve the effective locale for AI responses
    let locale = resolve_locale(config.locale.value.as_deref());
    let output_format = config.output_format.value;

    // Retry loop: on RequestTooLarge, drop the shortest man page reference and retry
    loop {
        // Determine if we have documentation to cite
        let with_citations = !references.is_empty();

        // Build schema and prompt dynamically based on whether we have docs
        let schema_value = build_explain_schema(with_citations);
        let system_prompt = build_system_prompt(with_citations, locale.as_deref());

        // Build system messages: instructions first, then man page documentation
        let mut system_messages = vec![system_prompt];
        for r in &references {
            system_messages.push(r.content.clone());
        }

        log::debug!("System messages: {} (1 instructions + {} man pages), User messages: 1",
                  system_messages.len(), references.len());

        let request = CompletionRequest {
            system_messages,
            message_history: vec![],
            user_message: command_to_explain.to_string(),
            json_schema: Some(schema_value),
            schema_name: "command_explanation".to_string(),
        };

        fn parse_explanation(resp: &CompletionResponse) -> Result<ExplainResult, String> {
            log::trace!("Raw model response ({} chars):\n{}", resp.content.len(), resp.content);
            serde_json::from_str::<ExplainResult>(&resp.content)
                .map_err(|e| format!("Failed to parse explanation JSON: {}", e))
        }

        // Try to create streaming preview (None if not TTY)
        let mut preview = StreamingPreview::new(config.preview_mode.value)?;

        let result = if let Some(ref mut preview) = preview {
            // Streaming mode with live preview
            let chunks_handle = preview.chunks_handle();
            let char_count_handle = preview.char_count_handle();
            let status_handle = preview.status_handle();
            let is_thinking_handle = preview.is_thinking_handle();
            let thinking_start_handle = preview.thinking_start_handle();
            let thinking_total_secs_handle = preview.thinking_total_secs_handle();

            let backend_clone = Arc::clone(&backend);
            let request_clone = request.clone();

            // Spawn the blocking backend call with correction harness
            let task = tokio::task::spawn_blocking(move || {
                complete_with_correction(
                    &*backend_clone,
                    &request_clone,
                    2,
                    |attempt| {
                        // On correction retry, clear visual content but keep char count
                        // (so the user can see that work was done)
                        if attempt.attempt > 0 {
                            if let Ok(mut chunks) = chunks_handle.lock() {
                                chunks.clear();
                            }
                            if let Ok(mut status) = status_handle.lock() {
                                *status = Some(format!(
                                    "correcting response ({}/{})",
                                    attempt.attempt, attempt.max_retries
                                ));
                            }
                        }

                        let chunks = chunks_handle.clone();
                        let char_count = char_count_handle.clone();
                        let status = status_handle.clone();
                        let is_thinking = is_thinking_handle.clone();
                        let thinking_start = thinking_start_handle.clone();
                        let thinking_total = thinking_total_secs_handle.clone();

                        Box::new(move |event| {
                            use crate::preview::{append_chunk, ChunkType};
                            match event {
                                StreamEvent::TextDelta(text) => {
                                    if let Ok(mut s) = status.lock() { *s = None; }
                                    let text_chars = text.chars().count();
                                    if let Ok(mut c) = chunks.lock() {
                                        append_chunk(&mut c, ChunkType::Content, text);
                                    }
                                    if let Ok(mut n) = char_count.lock() { *n += text_chars; }
                                    if let Ok(mut t) = is_thinking.lock() {
                                        if *t {
                                            *t = false;
                                            if let Ok(mut start) = thinking_start.lock() {
                                                if let Some(s) = start.take() {
                                                    if let Ok(mut total) = thinking_total.lock() {
                                                        *total += s.elapsed().as_secs_f64();
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                StreamEvent::Preamble(text) => {
                                    if let Ok(mut s) = status.lock() { *s = None; }
                                    let text_chars = text.chars().count();
                                    if let Ok(mut c) = chunks.lock() {
                                        append_chunk(&mut c, ChunkType::Preamble, text);
                                    }
                                    if let Ok(mut n) = char_count.lock() { *n += text_chars; }
                                    if let Ok(mut t) = is_thinking.lock() {
                                        if !*t {
                                            *t = true;
                                            if let Ok(mut start) = thinking_start.lock() {
                                                *start = Some(Instant::now());
                                            }
                                        }
                                    }
                                }
                                StreamEvent::Backoff { attempt, delay_ms } => {
                                    if let Ok(mut s) = status.lock() {
                                        let secs = delay_ms as f64 / 1000.0;
                                        *s = Some(format!("backoff #{}, {:.1}s", attempt, secs));
                                    }
                                }
                                StreamEvent::Retrying { attempt } => {
                                    if let Ok(mut s) = status.lock() {
                                        *s = Some(format!("retry #{}", attempt));
                                    }
                                }
                            }
                            StreamAction::Continue
                        })
                    },
                    parse_explanation,
                )
            });

            // Render loop while waiting for completion
            loop {
                preview.render()?;
                tokio::time::sleep(Duration::from_millis(80)).await;

                if task.is_finished() {
                    break;
                }
            }

            // Get the result
            task.await.map_err(|e| anyhow!("Task join error: {}", e))?
        } else {
            // Non-TTY mode: use simple progress indicator
            let progress = Progress::new("Generating explanation...");
            let backend_clone = Arc::clone(&backend);
            let request_clone = request.clone();

            let result = tokio::task::spawn_blocking(move || {
                complete_with_correction(
                    &*backend_clone,
                    &request_clone,
                    2,
                    |_| Box::new(|_| StreamAction::Continue),
                    parse_explanation,
                )
            }).await.map_err(|e| anyhow!("Task join error: {}", e))?;

            if let Some(p) = progress {
                p.finish_and_clear();
            }

            result
        };

        match result {
            Ok(explanation) => {
                // Clear streaming preview
                if let Some(ref mut p) = preview {
                    p.finish_and_clear()?;
                }

                // Render output based on output format from config
                match output_format {
                    OutputFormat::Json => {
                        println!("{}", serde_json::to_string_pretty(&explanation)?);
                    }
                    OutputFormat::Human => {
                        println!();
                        println!("{}", "Explanation:".white().bold());
                        println!();
                        println!("  {}", explanation.synopsis.dimmed());
                        println!();
                        for node in &explanation.explanations {
                            render_node(command_to_explain, node, 1);
                        }
                        println!();
                    }
                }

                return Ok(());
            }
            Err(BackendError::RequestTooLarge(msg)) => {
                // Clear preview before retry
                if let Some(ref mut p) = preview {
                    p.finish_and_clear()?;
                }

                log::debug!("Request too large: {}", msg);

                if references.is_empty() {
                    bail!("Request too large: {}", msg);
                }

                // Drop the shortest reference (first in sorted list) and retry
                let dropped = references.remove(0);
                log::info!(
                    "Context too large, dropping man page for '{}' and retrying...",
                    dropped.command
                );
                continue;
            }
            Err(e) => {
                // Clear preview before error
                if let Some(ref mut p) = preview {
                    p.finish_and_clear()?;
                }
                return Err(anyhow!("{}", e));
            }
        }
    }
}

fn render_node(original_command: &str, node: &ExplanationNode, indent: usize) {
    let indent_str = "  ".repeat(indent);

    // Build the line: {prefix} {segment} {suffix}
    let mut line = format!("{}• ", indent_str);
    if let Some(prefix) = &node.prefix {
        if !prefix.is_empty() {
            line.push_str(prefix);
            line.push(' ');
        }
    }

    // Handle potential double-escaping from the model: if segment isn't found
    // in the original command, try JSON-decoding it once more
    let segment = if original_command.contains(&node.segment) {
        node.segment.clone()
    } else if let Ok(decoded) = serde_json::from_str::<String>(&format!("\"{}\"", &node.segment)) {
        if original_command.contains(&decoded) {
            decoded
        } else {
            node.segment.clone()
        }
    } else {
        node.segment.clone()
    };

    line.push_str(&segment.cyan().to_string());

    if let Some(suffix) = &node.suffix {
        if !suffix.is_empty() {
            line.push(' ');
            line.push_str(suffix);
        }
    }

    println!("{}", line);

    for child in &node.children {
        render_node(original_command, child, indent + 1);
    }
}