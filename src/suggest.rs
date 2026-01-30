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

// Command selection options (dialog mode)
const SYSTEM_OPTION_GEN: &str = "Generate new suggestions";
const SYSTEM_OPTION_NEW: &str = "Enter a new command";
const SYSTEM_OPTION_DISMISS: &str = "Quit";

// Action menu options (after selecting a command)
const ACTION_COPY: &str = "Copy to clipboard";
const ACTION_EXPLAIN: &str = "Explain command";
const ACTION_EXECUTE: &str = "Execute command";
const ACTION_REVISE: &str = "Revise command";
const ACTION_EXIT: &str = "Quit";

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
            // Build selection menu with numbered options and letter shortcuts
            let mut select = InteractiveSelect::new("Select a command:");
            for (i, s) in suggestions.iter().enumerate() {
                let key = char::from_digit((i + 1) as u32, 10).unwrap_or('?');
                select = select.option(key, &s.command);
            }
            select = select
                .option('g', SYSTEM_OPTION_GEN)
                .option('n', SYSTEM_OPTION_NEW)
                .option('q', SYSTEM_OPTION_DISMISS);

            let selection = select.run().map_err(|e| anyhow!("Selection error: {}", e))?;

            match selection {
                Some('q') | None => return Ok(()),
                Some('n') => {
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
                Some('g') => continue 'outer, // Regenerate
                Some(c) => {
                    // Numeric selection
                    if let Some(idx) = c.to_digit(10) {
                        let idx = idx as usize;
                        if idx >= 1 && idx <= suggestions.len() {
                            let mut selected_command = suggestions[idx - 1].command.clone();

                            // Action menu loop
                            loop {
                                println!();
                                println!("Selected: {}", selected_command.green());

                                let mut action_select = InteractiveSelect::new("Action:")
                                    .option('c', ACTION_COPY)
                                    .option('e', ACTION_EXPLAIN)
                                    .option('x', ACTION_EXECUTE)
                                    .option('r', ACTION_REVISE)
                                    .option('b', "Back to suggestions")
                                    .option('q', ACTION_EXIT);

                                let action = action_select.run().map_err(|e| anyhow!("Selection error: {}", e))?;

                                match action {
                                    Some('c') => {
                                        ui::copy_to_clipboard(&selected_command);
                                    }
                                    Some('e') => {
                                        if let Err(e) = explain::explain_command(&selected_command, validated).await {
                                            log::error!("Failed to explain command: {}", e);
                                        }
                                    }
                                    Some('x') => {
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
                                    Some('r') => {
                                        if let Some(revised) = TextInput::new("Revise command:")
                                            .with_initial_value(&selected_command)
                                            .run()
                                            .map_err(|e| anyhow!("Input error: {}", e))?
                                        {
                                            selected_command = revised;
                                        }
                                    }
                                    Some('b') => continue 'selection, // Back to selection menu
                                    Some('q') | None => return Ok(()),
                                    _ => {}
                                }
                            }
                        }
                    }
                }
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
            println!("  {}. {}", "g".cyan(), "Generate new suggestions");
            println!("  {}. {}", "n".cyan(), "Enter new prompt");
            println!("  {}. {}", "q".cyan(), "Quit");
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
                        println!("  {}. {}", "c".cyan(), "Copy to clipboard");
                        println!("  {}. {}", "e".cyan(), "Explain command");
                        println!("  {}. {}", "x".cyan(), "Execute command");
                        println!("  {}. {}", "r".cyan(), "Revise command");
                        println!("  {}. {}", "b".cyan(), "Back to selection");
                        println!("  {}. {}", "q".cyan(), "Quit");
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
                            "q" | _ => {
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

    if command.starts_with("cd ") {
        let path = command[3..].trim();
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
        stdout[stdout.len() - max_len..].to_string()
    } else {
        stdout
    };
    *ctx_buffer = trimmed;

    if !output.status.success() {
        *ctx_enabled = false;
    }

    Ok(())
}
