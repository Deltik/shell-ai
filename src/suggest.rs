use std::io::{self, BufRead, Write};

use anyhow::{anyhow, Context, Result};
use colored::Colorize;
use futures::{stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::config::{Frontend, OutputFormat, ValidatedConfig};
use crate::explain;
use crate::http;
use crate::progress::Progress;
use crate::provider::ProviderConfig;
use crate::ui::{self, InteractiveSelect, TextInput};

#[derive(Debug, Deserialize, Serialize, Clone)]
struct Suggestion {
    command: String,
}

// Command selection options (dialog mode)
const SYSTEM_OPTION_GEN: &str = "Generate new suggestions";
const SYSTEM_OPTION_NEW: &str = "Enter a new command";
const SYSTEM_OPTION_DISMISS: &str = "Dismiss";

// Action menu options (after selecting a command)
const ACTION_COPY: &str = "Copy to clipboard";
const ACTION_EXPLAIN: &str = "Explain command";
const ACTION_EXECUTE: &str = "Execute command";
const ACTION_REVISE: &str = "Revise command";
const ACTION_EXIT: &str = "Exit";

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

pub async fn run_suggest(validated: &ValidatedConfig<'_>, opts: SuggestOptions) -> Result<()> {
    let prompt = opts.prompt.join(" ");
    if prompt.trim().is_empty() {
        println!("Describe what you want to do as a single sentence. `shai <sentence>`");
        return Ok(());
    }

    // Context mode flag (CLI or env var)
    let ctx_enabled = opts.ctx || matches!(std::env::var("CTX"), Ok(v) if v.to_lowercase() == "true");

    // Dispatch to appropriate frontend
    let config = validated.app_config();
    match config.frontend.value {
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
        // Show progress while generating suggestions
        let progress = Progress::new("Generating suggestions...");
        let suggestions = generate_suggestions(validated, &prompt, ctx_enabled, &ctx_buffer).await;
        if let Some(ref p) = progress {
            p.finish_and_clear();
        }
        let suggestions = suggestions?;

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
        // Show progress while generating suggestions
        let progress = Progress::new("Generating suggestions...");
        let suggestions = generate_suggestions(validated, &prompt, ctx_enabled, &ctx_buffer).await;
        if let Some(ref p) = progress {
            p.finish_and_clear();
        }
        let suggestions = suggestions?;

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
    let progress = Progress::new("Generating suggestions...");
    let suggestions = generate_suggestions(validated, prompt, false, "").await;
    if let Some(ref p) = progress {
        p.finish_and_clear();
    }
    let suggestions = suggestions?;

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
) -> Result<Vec<Suggestion>> {
    let config = validated.app_config();
    let count = config.suggestion_count.value.max(1) as usize;
    let max_workers = 4usize;

    let prompt_string = prompt.to_string();
    let ctx_string = if ctx_enabled { ctx_buffer.to_string() } else { String::new() };
    let prov = ProviderConfig::from_validated(validated);

    let tasks = stream::iter(0..count).map(|_| {
        let p = prompt_string.clone();
        let c = ctx_string.clone();
        let prov = prov.clone();
        async move { suggest_once(&prov, &p, &c).await }
    });

    let mut results: Vec<Suggestion> = Vec::new();
    let mut last_error: Option<String> = None;

    tasks
        .buffer_unordered(max_workers)
        .for_each(|res| {
            match res {
                Ok(Some(s)) if !s.command.trim().is_empty() => {
                    if !results.iter().any(|existing| existing.command == s.command) {
                        results.push(s);
                    }
                }
                Ok(Some(_)) => {} // Empty command, skip
                Ok(None) => {}    // No suggestion, skip
                Err(e) => {
                    log::debug!("Suggestion attempt failed: {}", e);
                    last_error = Some(e.to_string());
                }
            }
            futures::future::ready(())
        })
        .await;

    if results.is_empty() {
        let reason = last_error.unwrap_or_else(|| "unknown error".to_string());
        Err(anyhow!(
            "No suggestions could be generated.\nReason: {}",
            reason
        ))
    } else {
        Ok(results)
    }
}

async fn suggest_once(
    provider: &ProviderConfig,
    prompt: &str,
    ctx_buffer: &str,
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

    let schema_value: serde_json::Value = serde_json::from_str(SUGGEST_SCHEMA)
        .context("invalid internal suggest JSON schema")?;

    let mut payload = json!({
        "model": provider.model,
        "messages": [
            { "role": "system", "content": system_message },
            { "role": "user", "content": format!("Generate a shell command that satisfies this user request: {}", prompt) }
        ],
        "temperature": provider.temperature,
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "name": "shell_command_suggestion",
                "strict": true,
                "schema": schema_value
            }
        }
    });

    // Add max_tokens if configured
    if let Some(max_tokens) = provider.max_tokens {
        payload["max_tokens"] = json!(max_tokens);
    }

    let url = provider.chat_completions_url();
    let bearer_token = provider.api_key.as_deref();
    let extra_headers = provider.extra_headers_ref();

    let resp_json: serde_json::Value = http::post_json(&url, bearer_token, &extra_headers, &payload)?;

    if let Some(msg) = http::extract_api_error(&resp_json) {
        return Err(anyhow!("API error: {}", msg));
    }

    let content = http::extract_content_from_response(&resp_json)?;

    let suggestion: Suggestion = serde_json::from_str(content).map_err(|e| {
        // If parsing failed and response was truncated, give a helpful hint
        if http::is_truncated(&resp_json) {
            anyhow!(
                "Response truncated (max_tokens too low). Increase --max-tokens or SHAI_MAX_TOKENS."
            )
        } else {
            anyhow!("Failed to parse JSON from model: {}\nReceived: {}", e, content)
        }
    })?;

    Ok(Some(suggestion))
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