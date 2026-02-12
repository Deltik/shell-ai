use std::io::{self, BufRead, IsTerminal, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use colored::Colorize;
use futures::{stream, StreamExt};
use serde::{Deserialize, Serialize};

use crate::backend::{Backend, BackendError, CompletionRequest, StreamCallback, StreamEvent};
use crate::config::{resolve_locale, AppConfig, Frontend, OutputFormat, ValidatedConfig};
use crate::explain;
use crate::preview::SuggestProgress;
use crate::ui::{self, InteractiveSelect, TextInput};

#[derive(Debug, Deserialize, Serialize, Clone)]
struct Suggestion {
    command: String,
}

/// Incremental extractor for the `command` value from streaming JSON.
///
/// Processes partial JSON chunks and outputs only the decoded string contents
/// of the `"command"` key, stripping the `{"command":"…"}` wrapper.
struct CommandExtractor {
    state: ExtractState,
    /// Position within the target key `"command"`
    match_pos: usize,
}

#[derive(Clone, Copy)]
enum ExtractState {
    /// Looking for `"` that opens the key
    Scanning,
    /// Matching characters of `command"`
    MatchingKey,
    /// Found key, expecting `:`
    ExpectColon,
    /// Found `:`, expecting opening `"`
    ExpectQuote,
    /// Inside the string value, emitting characters
    InValue,
    /// Just saw `\` inside the value
    Escape,
    /// Closing `"` seen
    Done,
}

/// The key name to match, including the closing `"`.
/// E.g. for field `command`, this is `command"`.
const EXTRACT_KEY: &[u8] = b"command\"";

impl CommandExtractor {
    fn new() -> Self {
        // Exhaustive destructure: adding or removing fields from Suggestion
        // will cause a compile error here, forcing an update to the extractor.
        let _: fn(Suggestion) = |Suggestion { command: _ }| {};
        Self { state: ExtractState::Scanning, match_pos: 0 }
    }

    /// Feed a chunk of streaming JSON, returns the extracted command text (if any).
    fn feed(&mut self, chunk: &str) -> String {
        let mut out = String::new();
        for ch in chunk.chars() {
            match self.state {
                ExtractState::Scanning | ExtractState::MatchingKey => {
                    if ch.is_ascii() && self.match_pos < EXTRACT_KEY.len()
                        && EXTRACT_KEY[self.match_pos] == ch as u8
                    {
                        self.match_pos += 1;
                        self.state = if self.match_pos == EXTRACT_KEY.len() {
                            ExtractState::ExpectColon
                        } else {
                            ExtractState::MatchingKey
                        };
                    } else {
                        // Mismatch — reset, but check if ch is an opening quote
                        self.match_pos = 0;
                        self.state = if ch == '"' { ExtractState::MatchingKey }
                                     else { ExtractState::Scanning };
                    }
                }
                ExtractState::ExpectColon => match ch {
                    ':' => self.state = ExtractState::ExpectQuote,
                    c if c.is_ascii_whitespace() => {}
                    _ => { self.match_pos = 0; self.state = ExtractState::Scanning; }
                },
                ExtractState::ExpectQuote => match ch {
                    '"' => self.state = ExtractState::InValue,
                    c if c.is_ascii_whitespace() => {}
                    _ => { self.match_pos = 0; self.state = ExtractState::Scanning; }
                },
                ExtractState::InValue => match ch {
                    '"' => self.state = ExtractState::Done,
                    '\\' => self.state = ExtractState::Escape,
                    _ => out.push(ch),
                },
                ExtractState::Escape => {
                    out.push(match ch {
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        _ => ch, // handles \", \\, \/, and passthrough
                    });
                    self.state = ExtractState::InValue;
                }
                ExtractState::Done => {}
            }
        }
        out
    }
}

// Command selection system options (dialog mode)
#[derive(Clone, Copy, PartialEq)]
enum SystemOption {
    Generate,
    NewPrompt,
    Quit,
}

const SYSTEM_OPTIONS: &[(char, &str, SystemOption)] = &[
    ('g', "Generate new suggestions", SystemOption::Generate),
    ('n', "Enter a new command", SystemOption::NewPrompt),
    ('q', "Quit", SystemOption::Quit),
];

// Action menu options (after selecting a command)
#[derive(Clone, Copy, PartialEq)]
enum ActionOption {
    Copy,
    Explain,
    Execute,
    Revise,
    Back,
    Exit,
}

const ACTION_OPTIONS: &[(char, &str, ActionOption)] = &[
    ('c', "Copy to clipboard", ActionOption::Copy),
    ('e', "Explain command", ActionOption::Explain),
    ('x', "Execute command", ActionOption::Execute),
    ('r', "Revise command", ActionOption::Revise),
    ('b', "Back to suggestions", ActionOption::Back),
    ('q', "Quit", ActionOption::Exit),
];

/// JSON Schema for the `suggest` structured output.
const SUGGEST_SCHEMA: &str = r#"{
  "type": "object",
  "properties": {
    "command": {
      "type": "string",
      "description": "A single-line shell command that can be executed directly."
    }
  },
  "required": ["command"],
  "additionalProperties": false
}"#;

#[derive(Debug)]
pub struct SuggestOptions {
    pub ctx: bool,
    pub prompt: Vec<String>,
}

/// Resolve `Frontend::Automatic` to a concrete frontend based on runtime context.
///
/// Resolution rules:
/// - JSON output → Noninteractive (JSON requires structured output)
/// - TTY + Human output → Dialog (interactive menu)
/// - Non-TTY + Human output → Noninteractive (print first suggestion)
fn resolve_frontend(config: &AppConfig) -> Frontend {
    match config.frontend.value {
        Frontend::Automatic => {
            if config.output_format.value == OutputFormat::Json {
                Frontend::Noninteractive
            } else if std::io::stdout().is_terminal() {
                Frontend::Dialog
            } else {
                Frontend::Noninteractive
            }
        }
        other => other,
    }
}

pub async fn run_suggest(validated: &ValidatedConfig<'_>, opts: SuggestOptions) -> Result<()> {
    let prompt = opts.prompt.join(" ");
    if prompt.trim().is_empty() {
        println!("Describe what you want to do as a single sentence. `shai <sentence>`");
        return Ok(());
    }

    // Context mode flag (CLI or env var)
    let ctx_enabled = opts.ctx || matches!(std::env::var("CTX"), Ok(v) if v.to_lowercase() == "true");

    // Resolve automatic frontend to concrete frontend based on context
    let config = validated.app_config();
    let resolved_frontend = resolve_frontend(config);

    log::debug!(
        "Frontend resolution: {:?} -> {:?} (stdout_tty={}, output_format={:?})",
        config.frontend.value,
        resolved_frontend,
        std::io::stdout().is_terminal(),
        config.output_format.value
    );

    // Validate ctx compatibility with resolved frontend
    if resolved_frontend == Frontend::Noninteractive && ctx_enabled {
        return Err(anyhow!(
            "Context mode (--ctx) requires an interactive frontend.\n\
             The frontend resolved to noninteractive because stdout is not a TTY or JSON output was requested.\n\
             Hint: Run in a terminal with human output format to use context mode."
        ));
    }

    // Dispatch to appropriate frontend
    match resolved_frontend {
        Frontend::Automatic => unreachable!("Automatic should be resolved"),
        Frontend::Dialog => dialog_frontend(validated, &prompt, ctx_enabled).await,
        Frontend::Readline => readline_frontend(validated, &prompt, ctx_enabled).await,
        Frontend::Noninteractive => noninteractive_frontend(validated, &prompt).await,
    }
}

/// Dialog frontend using interactive menus with arrow keys and letter shortcuts.
async fn dialog_frontend(validated: &ValidatedConfig<'_>, initial_prompt: &str, mut ctx_enabled: bool) -> Result<()> {
    let mut prompt = initial_prompt.to_string();
    let mut ctx_buffer = String::new();

    if ctx_enabled {
        log::warn!(
            "Context mode enabled: command output will be sent to the AI provider. \
             Avoid running commands that output sensitive data. Disable with --ctx=false"
        );
        println!(">>> {}", std::env::current_dir()?.display());
    }

    'outer: loop {
        // Generate suggestions (streaming progress is shown internally)
        let suggestions = generate_suggestions(validated, &prompt, ctx_enabled, &ctx_buffer, None).await?;

        // Selection menu loop - allows returning here without regenerating
        'selection: loop {
            // Build selection menu with numbered options and system options
            let mut select = InteractiveSelect::new("Select a command:");
            for (i, s) in suggestions.iter().enumerate() {
                let key = char::from_digit((i + 1) as u32, 10).unwrap_or('?');
                select = select.option(key, &s.command);
            }
            // System options follow the suggestions
            let system_start_idx = suggestions.len();
            for (key, label, _) in SYSTEM_OPTIONS {
                select = select.option(*key, *label);
            }

            let selection = select.run().map_err(|e| anyhow!("Selection error: {}", e))?;

            // Determine what was selected
            let system_option = selection
                .filter(|&idx| idx >= system_start_idx)
                .and_then(|idx| SYSTEM_OPTIONS.get(idx - system_start_idx))
                .map(|(_, _, opt)| *opt);

            match (selection, system_option) {
                (None, _) => return Ok(()), // Cancelled (Esc/Ctrl+C/q)
                (_, Some(SystemOption::Quit)) => return Ok(()),
                (_, Some(SystemOption::NewPrompt)) => {
                    if let Some(new_prompt) = TextInput::new("New prompt:")
                        .run()
                        .map_err(|e| anyhow!("Input error: {}", e))?
                    {
                        prompt = new_prompt;
                        continue 'outer; // Regenerate with new prompt
                    }
                    // User cancelled - stay on selection menu
                    continue 'selection;
                }
                (_, Some(SystemOption::Generate)) => continue 'outer, // Regenerate
                (Some(idx), None) if idx < suggestions.len() => {
                    // Suggestion selected
                    let mut selected_command = suggestions[idx].command.clone();

                    // Action menu loop
                    loop {
                        println!();
                        println!("Selected: {}", selected_command.green());

                        let mut action_select = InteractiveSelect::new("Action:");
                        for (key, label, _) in ACTION_OPTIONS {
                            action_select = action_select.option(*key, *label);
                        }

                        let action_idx = action_select.run().map_err(|e| anyhow!("Selection error: {}", e))?;
                        let action = action_idx.and_then(|idx| ACTION_OPTIONS.get(idx)).map(|(_, _, opt)| *opt);

                        match action {
                            Some(ActionOption::Copy) => {
                                ui::copy_to_clipboard(&selected_command);
                            }
                            Some(ActionOption::Explain) => {
                                if let Err(e) = explain::explain_command(&selected_command, validated).await {
                                    log::error!("Failed to explain command: {}", e);
                                }
                            }
                            Some(ActionOption::Execute) => {
                                if !ctx_enabled {
                                    run_command_default(&selected_command)?;
                                    return Ok(());
                                } else {
                                    handle_command_with_ctx(&selected_command, &mut ctx_buffer, &mut ctx_enabled)?;
                                    println!(">>> {}", std::env::current_dir()?.display());
                                    if let Some(new_prompt) = TextInput::new("New prompt:")
                                        .run()
                                        .map_err(|e| anyhow!("Input error: {}", e))?
                                    {
                                        prompt = new_prompt;
                                    }
                                    continue 'outer; // Regenerate after execute in ctx mode
                                }
                            }
                            Some(ActionOption::Revise) => {
                                if let Some(revised) = TextInput::new("Revise command:")
                                    .with_initial_value(&selected_command)
                                    .run()
                                    .map_err(|e| anyhow!("Input error: {}", e))?
                                {
                                    selected_command = revised;
                                }
                            }
                            Some(ActionOption::Back) => continue 'selection,
                            Some(ActionOption::Exit) | None => return Ok(()),
                        }
                    }
                }
                _ => {} // Unknown selection, stay on menu
            }
        }
    }
}

/// Readline frontend using numbered selection and simple line input.
async fn readline_frontend(validated: &ValidatedConfig<'_>, initial_prompt: &str, mut ctx_enabled: bool) -> Result<()> {
    let mut prompt = initial_prompt.to_string();
    let mut ctx_buffer = String::new();

    if ctx_enabled {
        log::warn!(
            "Context mode enabled: command output will be sent to the AI provider. \
             Avoid running commands that output sensitive data. Disable with --ctx=false"
        );
        println!(">>> {}", std::env::current_dir()?.display());
    }

    let stdin = io::stdin();

    'outer: loop {
        // Generate suggestions (streaming progress is shown internally)
        let suggestions = generate_suggestions(validated, &prompt, ctx_enabled, &ctx_buffer, None).await?;

        // Selection loop - allows returning here without regenerating
        'selection: loop {
            // Print numbered list
            println!();
            for (i, s) in suggestions.iter().enumerate() {
                println!("  {}. {}", (i + 1).to_string().cyan(), s.command);
            }
            println!();
            println!("  {}. Generate new suggestions", "g".cyan());
            println!("  {}. Enter new prompt", "n".cyan());
            println!("  {}. Quit", "q".cyan());
            println!();

            print!("Select [1-{}/g/n/q]: ", suggestions.len());
            io::stdout().flush()?;

            let mut input = String::new();
            stdin.lock().read_line(&mut input)?;
            let input = input.trim().to_lowercase();

            if input == "q" {
                return Ok(());
            } else if input == "g" {
                continue 'outer; // Regenerate
            } else if input == "n" {
                print!("New prompt: ");
                io::stdout().flush()?;
                let mut new_prompt = String::new();
                stdin.lock().read_line(&mut new_prompt)?;
                prompt = new_prompt.trim().to_string();
                continue 'outer; // Regenerate with new prompt
            }

            // Try to parse as number
            if let Ok(num) = input.parse::<usize>() {
                if num >= 1 && num <= suggestions.len() {
                    let mut selected_command = suggestions[num - 1].command.clone();

                    // Action loop
                    loop {
                        println!();
                        println!("Selected: {}", selected_command.green());
                        println!();
                        println!("  {}. Copy to clipboard", "c".cyan());
                        println!("  {}. Explain command", "e".cyan());
                        println!("  {}. Execute command", "x".cyan());
                        println!("  {}. Revise command", "r".cyan());
                        println!("  {}. Back to selection", "b".cyan());
                        println!("  {}. Quit", "q".cyan());
                        println!();

                        print!("Action [c/e/x/r/b/q]: ");
                        io::stdout().flush()?;

                        let mut action_input = String::new();
                        stdin.lock().read_line(&mut action_input)?;
                        let action = action_input.trim().to_lowercase();

                        match action.as_str() {
                            "c" => {
                                ui::copy_to_clipboard(&selected_command);
                            }
                            "e" => {
                                if let Err(e) = explain::explain_command(&selected_command, validated).await {
                                    log::error!("Failed to explain command: {}", e);
                                }
                            }
                            "x" => {
                                if !ctx_enabled {
                                    run_command_default(&selected_command)?;
                                    return Ok(());
                                } else {
                                    handle_command_with_ctx(&selected_command, &mut ctx_buffer, &mut ctx_enabled)?;
                                    print!(">>> {}\nNew prompt: ", std::env::current_dir()?.display());
                                    io::stdout().flush()?;
                                    let mut new_prompt = String::new();
                                    stdin.lock().read_line(&mut new_prompt)?;
                                    prompt = new_prompt.trim().to_string();
                                    continue 'outer; // Regenerate after execute in ctx mode
                                }
                            }
                            "r" => {
                                print!("Revise command: ");
                                io::stdout().flush()?;
                                let mut revised = String::new();
                                stdin.lock().read_line(&mut revised)?;
                                let revised = revised.trim();
                                if !revised.is_empty() {
                                    selected_command = revised.to_string();
                                }
                            }
                            "b" => {
                                continue 'selection; // Back to selection menu
                            }
                            _ => {
                                return Ok(());
                            }
                        }
                    }
                }
            }

            println!("Invalid selection. Please try again.");
        }
    }
}

/// Noninteractive frontend: auto-select first suggestion and output.
async fn noninteractive_frontend(validated: &ValidatedConfig<'_>, prompt: &str) -> Result<()> {
    let config = validated.app_config();
    // Optimization: Only generate 1 suggestion for human output since we only use the first.
    // JSON output may want all suggestions for programmatic selection.
    let count_override = match config.output_format.value {
        OutputFormat::Human => Some(1),
        OutputFormat::Json => None,
    };
    // Generate suggestions (streaming progress is shown internally)
    let suggestions = generate_suggestions(validated, prompt, false, "", count_override).await?;

    match config.output_format.value {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&suggestions)?);
        }
        OutputFormat::Human => {
            if let Some(first) = suggestions.first() {
                println!("{}", first.command);
            }
        }
    }

    Ok(())
}

async fn generate_suggestions(
    validated: &ValidatedConfig<'_>,
    prompt: &str,
    ctx_enabled: bool,
    ctx_buffer: &str,
    count_override: Option<usize>,
) -> Result<Vec<Suggestion>> {
    let config = validated.app_config();
    let count = count_override.unwrap_or_else(|| config.suggestion_count.value.max(1) as usize);

    let prompt_string = prompt.to_string();
    let ctx_string = if ctx_enabled { ctx_buffer.to_string() } else { String::new() };
    let locale = resolve_locale(config.locale.value.as_deref());

    // Create backend once and share across parallel tasks
    let backend: Arc<dyn Backend> = Arc::from(validated.create_backend());

    // Create streaming progress (None if not TTY)
    let mut progress = SuggestProgress::new(count, config.preview_mode.value)?;
    let shared_slots = progress.as_ref().map(|p| p.shared_slots());

    // Spawn tasks with slot indices
    let tasks = stream::iter(0..count).map(|slot_idx| {
        let p = prompt_string.clone();
        let c = ctx_string.clone();
        let loc = locale.clone();
        let backend = Arc::clone(&backend);
        let slots = shared_slots.clone();

        async move {
            suggest_once_streaming(backend, &p, &c, loc.as_deref(), slot_idx, slots).await
        }
    });

    let mut results: Vec<Suggestion> = Vec::new();
    let mut last_error: Option<String> = None;

    // Use a channel to receive results while rendering
    let (tx, mut rx) = tokio::sync::mpsc::channel(count);

    // Spawn all tasks
    for task in tasks.collect::<Vec<_>>().await {
        let tx = tx.clone();
        tokio::spawn(async move {
            let result = task.await;
            let _ = tx.send(result).await;
        });
    }
    drop(tx); // Close sender so receiver knows when all tasks are done

    // Render loop while receiving results
    loop {
        tokio::select! {
            result = rx.recv() => {
                match result {
                    Some(Ok(Some(s))) if !s.command.trim().is_empty() => {
                        if !results.iter().any(|existing| existing.command == s.command) {
                            results.push(s);
                        }
                    }
                    Some(Ok(Some(_))) => {} // Empty command
                    Some(Ok(None)) => {}    // No suggestion
                    Some(Err(e)) => {
                        log::debug!("Suggestion attempt failed: {}", e);
                        last_error = Some(e.to_string());
                    }
                    None => {
                        // All tasks complete
                        break;
                    }
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                // Render progress if available
                if let Some(ref mut p) = progress {
                    p.render()?;
                }
            }
        }
    }

    // Final render and clear
    if let Some(ref mut p) = progress {
        p.finish_and_clear()?;
    }

    if results.is_empty() {
        let reason = last_error.unwrap_or_else(|| "unknown error".to_string());
        return Err(anyhow!(
            "No suggestions could be generated.\nReason: {}",
            reason
        ));
    }

    Ok(results)
}

/// Generate a single suggestion with streaming progress updates.
async fn suggest_once_streaming(
    backend: Arc<dyn Backend>,
    prompt: &str,
    ctx_buffer: &str,
    locale: Option<&str>,
    slot_idx: usize,
    shared_slots: Option<crate::preview::SharedSlots>,
) -> Result<Option<Suggestion>> {
    let mut system_message = String::from(
        "You are an expert at using shell commands. Respond with a JSON object only, \
         matching the provided JSON schema. The command will be directly executed \
         in a shell as a single executable line of code."
    );

    if !ctx_buffer.is_empty() {
        system_message.push_str(&format!(
            " Between [], these are the last 1500 characters from the previous \
             command's output, you can use them as context: [{}]",
            ctx_buffer
        ));
    }

    let platform_string = format!(
        " The system the shell command will be executed on is {} {}.",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    system_message.push_str(&platform_string);

    if let Some(loc) = locale {
        system_message.push_str(&format!(
            " Respond in the user's preferred locale/language: {}.",
            loc
        ));
    }

    let schema_value: serde_json::Value = serde_json::from_str(SUGGEST_SCHEMA)
        .context("invalid internal suggest JSON schema")?;

    let user_message = format!("Generate a shell command that satisfies this user request: {}", prompt);

    let request = CompletionRequest {
        system_messages: vec![system_message],
        user_message,
        json_schema: Some(schema_value),
        schema_name: "shell_command_suggestion".to_string(),
    };

    let callback_slots = shared_slots.clone();
    let extractor = Arc::new(Mutex::new(CommandExtractor::new()));
    let callback_extractor = extractor.clone();
    let callback: StreamCallback = Box::new(move |event| {
        match event {
            StreamEvent::TextDelta(text) => {
                if let Some(ref slots) = callback_slots {
                    let extracted = callback_extractor.lock().unwrap().feed(&text);
                    crate::preview::update_shared_slot(slots, slot_idx, &extracted);
                }
            }
            StreamEvent::Preamble(_) => {
                // Ignore preamble for suggest mode - we only care about the command output
            }
            StreamEvent::Backoff { attempt, delay_ms } => {
                if let Some(ref slots) = callback_slots {
                    crate::preview::backoff_shared_slot(slots, slot_idx, attempt, delay_ms);
                }
            }
            StreamEvent::Retrying { attempt } => {
                if let Some(ref slots) = callback_slots {
                    crate::preview::retrying_shared_slot(slots, slot_idx, attempt);
                }
            }
        }
    });

    // Use spawn_blocking with streaming
    let response = tokio::task::spawn_blocking(move || {
        backend.complete_streaming(&request, callback)
    })
    .await
    .map_err(|e| anyhow!("Task join error: {}", e))?;

    match response {
        Ok(resp) => {
            let suggestion: Suggestion = serde_json::from_str(&resp.content).map_err(|e| {
                if resp.is_truncated {
                    anyhow!(
                        "Response truncated (max_tokens too low). Increase --max-tokens or SHAI_MAX_TOKENS."
                    )
                } else {
                    anyhow!("Failed to parse JSON from model: {}\nReceived: {}", e, resp.content)
                }
            })?;

            // Mark slot as complete with the command (if TTY)
            if let Some(ref slots) = shared_slots {
                crate::preview::complete_shared_slot(slots, slot_idx, suggestion.command.clone());
            }
            Ok(Some(suggestion))
        }
        Err(e) => {
            // Mark slot as errored (if TTY)
            if let Some(ref slots) = shared_slots {
                crate::preview::error_shared_slot(slots, slot_idx, e.to_string());
            }
            Err(match e {
                BackendError::ApiError(msg) => anyhow!("API error: {}", msg),
                BackendError::RateLimited(msg) => anyhow!("Rate limited: {}", msg),
                BackendError::NetworkError(msg) => anyhow!("Network error: {}", msg),
                BackendError::ParseError(msg) => anyhow!("Parse error: {}", msg),
                BackendError::RequestTooLarge(msg) => anyhow!("Request too large: {}", msg),
                BackendError::Other(msg) => anyhow!("{}", msg),
            })
        }
    }
}

fn run_command_default(command: &str) -> Result<()> {
    #[cfg(windows)]
    let mut cmd = std::process::Command::new("cmd");
    #[cfg(not(windows))]
    let mut cmd = std::process::Command::new("sh");

    #[cfg(windows)]
    {
        cmd.arg("/C").arg(command);
    }
    #[cfg(not(windows))]
    {
        cmd.arg("-c").arg(command);
    }

    let status = cmd.status()?;
    if !status.success() {
        return Err(anyhow!("Command exited with status: {}", status));
    }
    Ok(())
}

fn handle_command_with_ctx(
    command: &str,
    ctx_buffer: &mut String,
    ctx_enabled: &mut bool,
) -> Result<()> {
    // Editors: do not capture their output.
    const TEXT_EDITORS: [&str; 9] = [
        "vi", "vim", "emacs", "nano", "ed", "micro", "joe", "nvim", "code",
    ];

    if TEXT_EDITORS.iter().any(|e| command.starts_with(e)) {
        run_command_default(command)?;
        return Ok(());
    }

    if let Some(path) = command.strip_prefix("cd ") {
        let path = path.trim();
        let expanded = shellexpand::tilde(path).into_owned();
        std::env::set_current_dir(expanded)?;
        return Ok(());
    }

    // Run command and capture stdout.
    #[cfg(windows)]
    let mut cmd = std::process::Command::new("cmd");
    #[cfg(not(windows))]
    let mut cmd = std::process::Command::new("sh");

    #[cfg(windows)]
    {
        cmd.arg("/C").arg(command);
    }
    #[cfg(not(windows))]
    {
        cmd.arg("-c").arg(command);
    }

    let output = cmd.output()?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    if !stdout.is_empty() {
        println!("\n{}", stdout);
    }

    // Update context buffer with last 1500 characters.
    let max_len = 1500usize;
    let trimmed = if stdout.len() > max_len {
        let start = stdout.len() - max_len;
        let start = (start..stdout.len())
            .find(|&i| stdout.is_char_boundary(i))
            .unwrap_or(stdout.len());
        stdout[start..].to_string()
    } else {
        stdout
    };
    *ctx_buffer = trimmed;

    if !output.status.success() {
        *ctx_enabled = false;
    }

    Ok(())
}
