use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::Path;

mod config;
mod explain;
mod http;
mod integration;
mod logger;
mod progress;
mod provider;
mod suggest;
mod ui;

use crate::config::{AppConfig, CliOverrides, DebugLevel, OutputFormat};

/// Global options available on all commands.
#[derive(Parser, Debug, Clone, Default)]
pub struct GlobalOptions {
    /// Provider override (openai, azure, groq, mistral, ollama)
    #[arg(long = "provider", global = true)]
    pub provider: Option<String>,

    /// Model override (provider-specific)
    #[arg(long = "model", global = true)]
    pub model: Option<String>,

    /// Max tokens for an AI completion
    #[arg(long = "max-tokens", global = true)]
    pub max_tokens: Option<u32>,

    /// Sampling temperature override
    #[arg(long = "temperature", global = true)]
    pub temperature: Option<f32>,

    /// Frontend mode: automatic (default), dialog, readline, or noninteractive
    #[arg(long = "frontend", global = true)]
    pub frontend: Option<String>,

    /// Output format: human, json
    #[arg(long = "output-format", global = true)]
    pub output_format: Option<String>,

    /// Enable debug output (prints debug info to stderr).
    /// Use --debug for debug level, --debug=trace for trace level.
    #[arg(long = "debug", short = 'd', global = true, value_enum, value_name = "LEVEL", num_args = 0..=1, default_missing_value = "debug", require_equals = true)]
    pub debug: Option<DebugLevel>,

    /// Language/locale for AI responses (auto-detected by default, empty string to disable)
    #[arg(long = "locale", global = true)]
    pub locale: Option<String>,
}

/// Shell-AI CLI (full interface with subcommands)
#[derive(Parser, Debug)]
#[command(
    name = "shell-ai",
    version = env!("GIT_VERSION"),
    about = "Shell-AI: AI-assisted shell commands",
    author = "Shell-AI contributors",
    subcommand_required = true,
    arg_required_else_help = true
)]
struct Cli {
    #[command(flatten)]
    global: GlobalOptions,

    #[command(subcommand)]
    command: Command,
}

/// Shorthand CLI for suggest mode (when invoked as `shai`)
#[derive(Parser, Debug)]
#[command(
    name = "shai",
    version = env!("GIT_VERSION"),
    about = "Shell-AI: AI-assisted shell command suggestions",
    author = "Shell-AI contributors"
)]
struct ShaiCli {
    #[command(flatten)]
    global: GlobalOptions,

    /// Enable context mode: sends previous command output to the AI for contextual follow-up suggestions. Note: output is sent to your AI provider.
    #[arg(long = "ctx")]
    ctx: bool,

    /// Prompt describing what you want to do.
    #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
    prompt: Vec<String>,
}

/// Top-level subcommands for the Shell-AI CLI.
#[derive(Subcommand, Debug)]
enum Command {
    /// Suggest shell commands from a natural-language description.
    Suggest(SuggestArgs),

    /// Explain an existing shell command using an OpenAI-compatible API.
    Explain(ExplainArgs),

    /// Configuration management.
    Config(ConfigArgs),

    /// Generate shell integration scripts (completions, aliases, keybindings).
    Integration(integration::IntegrationArgs),
}

#[derive(Parser, Debug)]
struct ConfigArgs {
    #[command(subcommand)]
    action: Option<ConfigAction>,
}

#[derive(Subcommand, Debug)]
enum ConfigAction {
    /// Generate a documented example config.toml.
    Init(ConfigInitArgs),

    /// Show configuration schema (descriptions of all settings).
    Schema,
}

#[derive(Parser, Debug)]
struct ConfigInitArgs {
    /// Print to stdout instead of writing to file.
    #[arg(long = "stdout")]
    stdout: bool,
}

#[derive(Parser, Debug)]
struct SuggestArgs {
    /// Enable context mode: sends previous command output to the AI for contextual follow-up suggestions. Note: output is sent to your AI provider.
    #[arg(long = "ctx")]
    ctx: bool,

    /// Prompt describing what you want to do.
    #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
    prompt: Vec<String>,
}

#[derive(Parser, Debug)]
struct ExplainArgs {
    /// Command to explain. If omitted and stdin is piped, read from stdin.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    command: Vec<String>,
}

/// Check if we were invoked as `shai` (shorthand for suggest)
fn invoked_as_shai() -> bool {
    std::env::args()
        .next()
        .and_then(|arg| {
            Path::new(&arg)
                .file_name()
                .map(|name| name.to_string_lossy().starts_with("shai"))
        })
        .unwrap_or(false)
}

/// Convert global CLI options to CliOverrides for config loading.
fn global_to_cli_overrides(global: &GlobalOptions) -> CliOverrides {
    CliOverrides {
        provider: global.provider.clone(),
        model: global.model.clone(),
        max_tokens: global.max_tokens,
        temperature: global.temperature,
        frontend: global.frontend.clone(),
        output_format: global.output_format.clone(),
        debug: global.debug,
        locale: global.locale.clone(),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    logger::init();

    // Parse CLI, converting `shai` shorthand to full Cli with Command::Suggest
    let cli = if invoked_as_shai() {
        let args = ShaiCli::parse();
        Cli {
            global: args.global,
            command: Command::Suggest(SuggestArgs {
                ctx: args.ctx,
                prompt: args.prompt,
            }),
        }
    } else {
        Cli::parse()
    };

    let cli_overrides = global_to_cli_overrides(&cli.global);
    let config = AppConfig::load_with_cli(cli_overrides);
    logger::set_debug(config.debug.value);

    match cli.command {
        Command::Suggest(args) => {
            let validated_config = config.validate()?;

            let opts = suggest::SuggestOptions {
                ctx: args.ctx,
                prompt: args.prompt,
            };
            suggest::run_suggest(&validated_config, opts).await?;
        }
        Command::Explain(args) => {
            let validated_config = config.validate()?;
            let opts = explain::ExplainOptions {
                command: args.command,
            };
            explain::run_explain(&validated_config, opts).await?;
        }
        Command::Config(args) => {
            if let Some(action) = args.action {
                match action {
                    ConfigAction::Init(init_args) => {
                        AppConfig::write_init_config(init_args.stdout)?;
                    }
                    ConfigAction::Schema => {
                        AppConfig::print_schema(config.output_format.value);
                    }
                }
            } else {
                // Default: print current config
                match config.output_format.value {
                    OutputFormat::Human => config.print_human(),
                    OutputFormat::Json => config.print_json(),
                }
            }
        }
        Command::Integration(args) => {
            integration::run(args, config.output_format.value)?;
        }
    }

    Ok(())
}