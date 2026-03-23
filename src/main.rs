use anyhow::Result;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use std::path::Path;

mod backend;
mod config;
mod explain;
mod http;
mod integration;
mod logger;
mod preview;
mod progress;
mod render;
mod suggest;
mod ui;

use crate::config::{AppConfig, CliOverrides, DebugLevel, Frontend, OutputFormat, PreviewMode, Provider};

/// Options available on all commands.
#[derive(Parser, Debug, Clone, Default)]
pub struct GlobalOptions {
    /// Output format
    #[arg(long = "output-format", global = true, value_enum)]
    pub output_format: Option<OutputFormat>,

    /// Enable debug output (prints debug info to stderr).
    /// Use --debug for debug level, --debug=trace for trace level
    #[arg(long = "debug", short = 'd', global = true, value_enum, value_name = "LEVEL", num_args = 0..=1, default_missing_value = "debug", require_equals = true)]
    pub debug: Option<DebugLevel>,
}

/// Configuration overrides for AI-related settings.
/// Flattened only into subcommands that use them.
#[derive(Parser, Debug, Clone, Default)]
#[command(next_help_heading = "Configuration Overrides")]
pub struct ConfigOverrides {
    /// Provider override
    #[arg(long = "provider", value_enum)]
    pub provider: Option<Provider>,

    /// Model override (provider-specific)
    #[arg(long = "model")]
    pub model: Option<String>,

    /// Max tokens for an AI completion
    #[arg(long = "max-tokens")]
    pub max_tokens: Option<u32>,

    /// Sampling temperature override
    #[arg(long = "temperature")]
    pub temperature: Option<f32>,

    /// Frontend mode
    #[arg(long = "frontend", value_enum)]
    pub frontend: Option<Frontend>,

    /// Maximum preview display mode
    #[arg(long = "preview-mode", short = 'P', value_enum)]
    pub preview_mode: Option<PreviewMode>,

    /// Language/locale for AI responses (auto-detected by default, empty string to disable)
    #[arg(long = "locale")]
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

    #[command(flatten)]
    overrides: ConfigOverrides,

    /// Enable context mode: sends previous command output to the AI for contextual follow-up suggestions. Note: output is sent to your AI provider
    #[arg(long = "ctx")]
    ctx: bool,

    /// Prompt describing what you want to do
    #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
    prompt: Vec<String>,
}

/// Top-level subcommands for the Shell-AI CLI.
#[derive(Subcommand, Debug)]
enum Command {
    /// Suggest shell commands from a natural-language description
    Suggest(SuggestArgs),

    /// Explain a shell command in plain language
    Explain(ExplainArgs),

    /// Show, initialize, or inspect configuration
    Config(ConfigArgs),

    /// Generate shell integration scripts (completions, aliases, keybindings)
    Integration(integration::IntegrationArgs),
}

#[derive(Parser, Debug)]
struct ConfigArgs {
    #[command(flatten)]
    overrides: ConfigOverrides,

    #[command(subcommand)]
    action: Option<ConfigAction>,
}

#[derive(Subcommand, Debug)]
enum ConfigAction {
    /// Generate a documented example config.toml
    Init(ConfigInitArgs),

    /// Show configuration schema (descriptions of all settings)
    Schema,
}

#[derive(Parser, Debug)]
struct ConfigInitArgs {
    /// Print to stdout instead of writing to file
    #[arg(long = "stdout")]
    stdout: bool,
}

#[derive(Parser, Debug)]
#[command(after_long_help = "\
Examples:\n  \
  shell-ai suggest -- 'list files larger than 100MB'\n  \
  shell-ai suggest -- find and kill process on port 8080")]
struct SuggestArgs {
    /// Enable context mode: sends previous command output to the AI for contextual follow-up suggestions. Note: output is sent to your AI provider
    #[arg(long = "ctx")]
    ctx: bool,

    /// Prompt describing what you want to do
    #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
    prompt: Vec<String>,

    #[command(flatten)]
    overrides: ConfigOverrides,
}

#[derive(Parser, Debug)]
#[command(after_long_help = "\
Examples:\n  \
  shell-ai explain -- tar -xzf archive.tar.gz\n  \
  shell-ai explain -- 'find . -name \"*.log\" -mtime +7 -delete'\n  \
  history | tail -1 | shell-ai explain")]
struct ExplainArgs {
    /// Command to explain. If omitted and stdin is piped, read from stdin
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    command: Vec<String>,

    #[command(flatten)]
    overrides: ConfigOverrides,
}

/// Apply runtime help text to subcommand trees.
fn augment_subcommand_help(cmd: clap::Command) -> clap::Command {
    let toml_path = config::toml_config_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<unknown>".to_string());
    let json_path = config::json_config_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<unknown>".to_string());

    let config_long_about = "Show, initialize, or inspect configuration.\n\n\
         When run without a subcommand, displays the active configuration \
         with the source of each value (default, toml, json, env, cli).".to_string();
    let init_long_about = format!(
        "Generate a documented example config.toml.\n\n\
         Writes to {toml_path} by default.\n\
         A legacy JSON config at {json_path} is also loaded if present \
         and takes precedence over the TOML file."
    );

    cmd.mut_subcommand("config", |config_cmd| {
        config_cmd
            .long_about(config_long_about)
            .after_long_help(
                "Examples:\n  \
                 shell-ai config\n  \
                 shell-ai config init\n  \
                 shell-ai config init --stdout\n  \
                 shell-ai config schema",
            )
            .mut_subcommand("init", |init_cmd| init_cmd.long_about(init_long_about))
    })
    .mut_subcommand("integration", |int_cmd| {
        int_cmd
            .after_long_help(
                "Examples:\n  \
                 shell-ai integration generate bash\n  \
                 shell-ai integration generate zsh --preset full\n  \
                 shell-ai integration list\n  \
                 shell-ai integration update",
            )
            .mut_subcommand("generate", |gen_cmd| {
                gen_cmd.after_long_help(
                    "Examples:\n  \
                     shell-ai integration generate bash\n  \
                     shell-ai integration generate zsh --preset full\n  \
                     shell-ai integration generate fish --add keybinding\n  \
                     shell-ai integration generate bash --remove aliases --stdout",
                )
            })
    })
}

/// Check if an executable named `shai` exists in PATH.
fn shai_in_path() -> bool {
    let name = if cfg!(windows) { "shai.exe" } else { "shai" };
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| {
                let path = dir.join(name);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    path.metadata()
                        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
                        .unwrap_or(false)
                }
                #[cfg(not(unix))]
                {
                    path.is_file()
                }
            })
        })
        .unwrap_or(false)
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

/// Build CliOverrides from global options and optional per-subcommand config overrides.
fn build_cli_overrides(global: &GlobalOptions, overrides: Option<&ConfigOverrides>) -> CliOverrides {
    CliOverrides {
        provider: overrides.and_then(|o| o.provider.map(|p| p.to_string())),
        model: overrides.and_then(|o| o.model.clone()),
        max_tokens: overrides.and_then(|o| o.max_tokens),
        temperature: overrides.and_then(|o| o.temperature),
        frontend: overrides.and_then(|o| o.frontend.map(|f| f.to_string())),
        output_format: global.output_format.map(|o| o.to_string()),
        preview_mode: overrides.and_then(|o| o.preview_mode),
        debug: global.debug,
        locale: overrides.and_then(|o| o.locale.clone()),
    }
}

/// Extract ConfigOverrides from the active subcommand, if present.
fn extract_config_overrides(command: &Command) -> Option<&ConfigOverrides> {
    match command {
        Command::Suggest(args) => Some(&args.overrides),
        Command::Explain(args) => Some(&args.overrides),
        Command::Config(args) => Some(&args.overrides),
        Command::Integration(_) => None,
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
                overrides: args.overrides,
                ctx: args.ctx,
                prompt: args.prompt,
            }),
        }
    } else {
        let mut cmd = Cli::command();
        if !shai_in_path() {
            cmd = cmd.after_long_help("\
Tip: Create a symlink or copy named 'shai' for a shorthand that goes \
straight to suggest mode:\n\n  \
  ln -s shell-ai shai\n  \
  shai -- 'list files larger than 100MB'");
        }
        cmd = augment_subcommand_help(cmd);
        let matches = cmd.get_matches();
        Cli::from_arg_matches(&matches)?
    };

    let cli_overrides = build_cli_overrides(&cli.global, extract_config_overrides(&cli.command));
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