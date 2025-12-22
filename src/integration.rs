//! Shell integration generation for shell-ai.
//!
//! Generates integration scripts with configurable features:
//! - completions: Tab completion for shell-ai commands
//! - aliases: ?? for suggest, explain for explain
//! - keybinding: Ctrl+G inline transform with progress indicator

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{generate, Shell as ClapShell};
use colored::Colorize;
use serde::Serialize;
use strum::{Display, EnumIter, IntoEnumIterator};

use crate::config::OutputFormat;
use crate::Cli;

/// Arguments for the integration subcommand.
#[derive(Parser, Debug)]
pub struct IntegrationArgs {
    #[command(subcommand)]
    pub action: IntegrationAction,
}

/// Integration subcommand actions.
#[derive(Subcommand, Debug)]
pub enum IntegrationAction {
    /// Generate a new integration script.
    Generate(IntegrationGenerateArgs),
    /// Update existing integration script(s) using stored preferences.
    Update(IntegrationUpdateArgs),
    /// Show available features, presets, and installed integrations.
    List,
}

#[derive(Parser, Debug)]
pub struct IntegrationGenerateArgs {
    /// Target shell: bash, zsh, fish, powershell
    #[arg(value_enum)]
    pub shell: ShellType,

    /// Base preset: minimal (completions only), standard (completions + aliases), full (all features)
    #[arg(long, short = 'p', default_value = "standard")]
    pub preset: Preset,

    /// Add feature(s) on top of preset. Can be specified multiple times.
    #[arg(long = "add", short = 'a', value_name = "FEATURE")]
    pub add_features: Vec<Feature>,

    /// Remove feature(s) from preset. Can be specified multiple times.
    #[arg(long = "remove", short = 'r', value_name = "FEATURE")]
    pub remove_features: Vec<Feature>,

    /// Print to stdout instead of writing to file.
    #[arg(long)]
    pub stdout: bool,

    /// Overwrite existing file without confirmation.
    #[arg(long, short = 'y')]
    pub overwrite: bool,
}

#[derive(Parser, Debug)]
pub struct IntegrationUpdateArgs {
    /// Target shell. If omitted, updates all existing integration files.
    #[arg(value_enum)]
    pub shell: Option<ShellType>,
}

/// Supported shell types.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Display, EnumIter)]
#[strum(serialize_all = "lowercase")]
pub enum ShellType {
    Bash,
    Zsh,
    Fish,
    #[clap(name = "powershell")]
    #[strum(serialize = "powershell")]
    PowerShell,
}

impl ShellType {
    /// Get the file extension for this shell.
    pub fn extension(&self) -> &'static str {
        match self {
            ShellType::Bash => "bash",
            ShellType::Zsh => "zsh",
            ShellType::Fish => "fish",
            ShellType::PowerShell => "ps1",
        }
    }

    /// Get the rc file path suggestion for this shell.
    pub fn rc_file(&self) -> &'static str {
        match self {
            ShellType::Bash => "~/.bashrc",
            ShellType::Zsh => "~/.zshrc",
            ShellType::Fish => "~/.config/fish/config.fish",
            ShellType::PowerShell => "$PROFILE",
        }
    }
}

impl FromStr for ShellType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "bash" => Ok(ShellType::Bash),
            "zsh" => Ok(ShellType::Zsh),
            "fish" => Ok(ShellType::Fish),
            "powershell" => Ok(ShellType::PowerShell),
            _ => Err(format!("Unknown shell: {}", s)),
        }
    }
}

/// Feature presets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Display, EnumIter)]
#[strum(serialize_all = "lowercase")]
pub enum Preset {
    /// Completions only
    Minimal,
    /// Completions + aliases
    Standard,
    /// Completions + aliases + keybinding
    Full,
}

impl Preset {
    /// Returns the set of features included in this preset.
    pub fn features(&self) -> HashSet<Feature> {
        match self {
            Preset::Minimal => [Feature::Completions].into_iter().collect(),
            Preset::Standard => [Feature::Completions, Feature::Aliases]
                .into_iter()
                .collect(),
            Preset::Full => [Feature::Completions, Feature::Aliases, Feature::Keybinding]
                .into_iter()
                .collect(),
        }
    }
}

impl FromStr for Preset {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "minimal" => Ok(Preset::Minimal),
            "standard" => Ok(Preset::Standard),
            "full" => Ok(Preset::Full),
            _ => Err(format!("Unknown preset: {}", s)),
        }
    }
}

/// Individual features that can be enabled/disabled.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, ValueEnum, Display, EnumIter)]
#[strum(serialize_all = "lowercase")]
pub enum Feature {
    /// Tab completion for shell-ai commands
    Completions,
    /// ?? and explain aliases/abbreviations
    Aliases,
    /// Ctrl+G keybinding for inline transform
    Keybinding,
}

impl FromStr for Feature {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "completions" => Ok(Feature::Completions),
            "aliases" => Ok(Feature::Aliases),
            "keybinding" => Ok(Feature::Keybinding),
            _ => Err(format!("Unknown feature: {}", s)),
        }
    }
}

/// Stored preferences parsed from an integration file header.
struct IntegrationPreferences {
    shell: ShellType,
    preset: Preset,
    add: Vec<Feature>,
    remove: Vec<Feature>,
}

// =============================================================================
// JSON output structures
// =============================================================================

#[derive(Serialize)]
struct IntegrationListJson {
    features: Vec<FeatureInfo>,
    presets: Vec<PresetInfo>,
    shells: Vec<String>,
    installed: Vec<InstalledIntegration>,
}

#[derive(Serialize)]
struct FeatureInfo {
    name: String,
    description: String,
}

#[derive(Serialize)]
struct PresetInfo {
    name: String,
    features: Vec<String>,
}

#[derive(Serialize)]
struct InstalledIntegration {
    shell: String,
    preset: String,
    features: Vec<String>,
    path: String,
}

/// Resolve final feature set from preset + modifiers.
fn resolve_features(preset: Preset, add: &[Feature], remove: &[Feature]) -> HashSet<Feature> {
    let mut features = preset.features();
    for f in add {
        features.insert(*f);
    }
    for f in remove {
        features.remove(f);
    }
    features
}

/// Get the integration file path for a shell.
fn integration_file_path(shell: ShellType) -> Option<PathBuf> {
    let mut base = dirs::config_dir()?;
    base.push("shell-ai");
    base.push(format!("integration.{}", shell.extension()));
    Some(base)
}

/// Format modifiers as +feature,-feature string.
fn format_modifiers(add: &[Feature], remove: &[Feature]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut add_sorted: Vec<_> = add.to_vec();
    add_sorted.sort_by_key(|f| f.to_string());
    let mut remove_sorted: Vec<_> = remove.to_vec();
    remove_sorted.sort_by_key(|f| f.to_string());

    for f in add_sorted {
        parts.push(format!("+{}", f));
    }
    for f in remove_sorted {
        parts.push(format!("-{}", f));
    }
    parts.join(",")
}

/// Generate the header section with metadata for update command.
fn generate_header(
    shell: ShellType,
    preset: Preset,
    add: &[Feature],
    remove: &[Feature],
) -> String {
    let version = env!("CARGO_PKG_VERSION");
    let modifiers = format_modifiers(add, remove);

    format!(
        r#"# shell-ai integration
# Generated by shell-ai v{version}
#
# DO NOT EDIT THIS FILE MANUALLY
# Regenerate with: shell-ai integration update {shell}
#
# @shell: {shell}
# @preset: {preset}
# @modifiers: {modifiers}
#
"#,
        version = version,
        shell = shell.to_string(),
        preset = preset.to_string(),
        modifiers = modifiers,
    )
}

/// Parse modifiers from +feature,-feature format.
/// Returns an error message if an unknown feature is encountered.
fn parse_modifiers(value: &str) -> Result<(Vec<Feature>, Vec<Feature>), String> {
    let mut add = Vec::new();
    let mut remove = Vec::new();

    for part in value.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some(name) = part.strip_prefix('+') {
            let feature = <Feature as FromStr>::from_str(name)
                .map_err(|_| format!("Unknown feature in modifiers: {}", name))?;
            add.push(feature);
        } else if let Some(name) = part.strip_prefix('-') {
            let feature = <Feature as FromStr>::from_str(name)
                .map_err(|_| format!("Unknown feature in modifiers: {}", name))?;
            remove.push(feature);
        }
    }

    Ok((add, remove))
}

/// Parse header to extract stored preferences.
/// Returns None if the header format is unrecognized.
/// Returns Some with an error inside if the header is recognized but has invalid content.
fn parse_header(content: &str) -> Result<IntegrationPreferences, String> {
    if !content.starts_with("# shell-ai integration") {
        return Err("Not a shell-ai integration file".to_string());
    }

    let mut shell = None;
    let mut preset = None;
    let mut add = Vec::new();
    let mut remove = Vec::new();

    for line in content.lines().take(15) {
        if let Some(value) = line.strip_prefix("# @shell: ") {
            shell = Some(
                <ShellType as FromStr>::from_str(value.trim())
                    .map_err(|e| format!("Invalid shell: {}", e))?,
            );
        } else if let Some(value) = line.strip_prefix("# @preset: ") {
            preset = Some(
                <Preset as FromStr>::from_str(value.trim())
                    .map_err(|e| format!("Invalid preset: {}", e))?,
            );
        } else if let Some(value) = line.strip_prefix("# @modifiers: ") {
            let (a, r) = parse_modifiers(value)?;
            add = a;
            remove = r;
        }
    }

    Ok(IntegrationPreferences {
        shell: shell.ok_or("Missing @shell in header")?,
        preset: preset.ok_or("Missing @preset in header")?,
        add,
        remove,
    })
}

/// Generate shell completions using clap_complete.
fn generate_completions(shell: ShellType) -> String {
    let mut cmd = Cli::command();
    let clap_shell = match shell {
        ShellType::Bash => ClapShell::Bash,
        ShellType::Zsh => ClapShell::Zsh,
        ShellType::Fish => ClapShell::Fish,
        ShellType::PowerShell => ClapShell::PowerShell,
    };

    let mut buf = Vec::new();
    generate(clap_shell, &mut cmd, "shell-ai", &mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

/// Generate the full integration file content.
fn generate_integration_file(
    shell: ShellType,
    preset: Preset,
    add: &[Feature],
    remove: &[Feature],
) -> String {
    let features = resolve_features(preset, add, remove);
    let mut output = generate_header(shell, preset, add, remove);

    match shell {
        ShellType::Bash => {
            if features.contains(&Feature::Completions) {
                output.push_str("\n# === Completions ===\n");
                output.push_str(&generate_completions(shell));
            }
            if features.contains(&Feature::Aliases) {
                output.push_str(BASH_ALIASES);
            }
            if features.contains(&Feature::Keybinding) {
                output.push_str(BASH_KEYBINDING);
            }
        }
        ShellType::Zsh => {
            if features.contains(&Feature::Completions) {
                output.push_str("\n# === Completions ===\n");
                output.push_str(&generate_completions(shell));
            }
            if features.contains(&Feature::Aliases) {
                output.push_str(ZSH_ALIASES);
            }
            if features.contains(&Feature::Keybinding) {
                output.push_str(ZSH_KEYBINDING);
            }
        }
        ShellType::Fish => {
            if features.contains(&Feature::Completions) {
                output.push_str("\n# === Completions ===\n");
                output.push_str(&generate_completions(shell));
            }
            if features.contains(&Feature::Aliases) {
                output.push_str(FISH_ALIASES);
            }
            if features.contains(&Feature::Keybinding) {
                output.push_str(FISH_KEYBINDING);
            }
        }
        ShellType::PowerShell => {
            if features.contains(&Feature::Completions) {
                output.push_str("\n# === Completions ===\n");
                output.push_str(&generate_completions(shell));
            }
            if features.contains(&Feature::Aliases) {
                output.push_str(POWERSHELL_ALIASES);
            }
            if features.contains(&Feature::Keybinding) {
                output.push_str(POWERSHELL_KEYBINDING);
            }
        }
    }

    output
}

/// Replace home directory with $HOME for portable paths.
fn path_with_home_var(path: &PathBuf) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(relative) = path.strip_prefix(&home) {
            return format!("$HOME/{}", relative.display());
        }
    }
    path.display().to_string()
}

/// Print sourcing instructions for the user.
fn print_sourcing_instructions(shell: ShellType, path: &PathBuf) {
    let path_str = path_with_home_var(path);

    println!(
        "\nAdd this to your shell configuration ({}):\n",
        shell.rc_file().cyan()
    );

    match shell {
        ShellType::Bash | ShellType::Zsh | ShellType::Fish => {
            println!("  [ -f \"{}\" ] && source \"{}\"", path_str, path_str);
        }
        ShellType::PowerShell => {
            println!(
                "  if (Test-Path \"{}\") {{ . \"{}\" }}",
                path_str, path_str
            );
        }
    }
    println!();
}

/// Run the generate action.
pub fn run_generate(args: IntegrationGenerateArgs) -> Result<()> {
    // Validate feature combinations
    let features = resolve_features(args.preset, &args.add_features, &args.remove_features);

    if features.is_empty() {
        anyhow::bail!(
            "No features selected. The preset '{}' with your modifiers results in an empty feature set.\n\
             Available features: {}",
            args.preset,
            Feature::iter()
                .map(|f| f.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    // Generate content
    let content = generate_integration_file(
        args.shell,
        args.preset,
        &args.add_features,
        &args.remove_features,
    );

    // Handle output
    if args.stdout {
        print!("{}", content);
        return Ok(());
    }

    let path = integration_file_path(args.shell).ok_or_else(|| {
        anyhow::anyhow!("Could not determine config directory. Try using --stdout instead.")
    })?;

    // Check for existing file
    if path.exists() && !args.overwrite {
        if let Ok(existing_content) = fs::read_to_string(&path) {
            if let Ok(existing_prefs) = parse_header(&existing_content) {
                let existing_features = resolve_features(
                    existing_prefs.preset,
                    &existing_prefs.add,
                    &existing_prefs.remove,
                );
                let mut existing_features: Vec<_> =
                    existing_features.iter().map(|f| f.to_string()).collect();
                existing_features.sort();
                let mut new_features: Vec<_> = features.iter().map(|f| f.to_string()).collect();
                new_features.sort();

                anyhow::bail!(
                    "Integration file already exists: {}\n\n\
                     Current: preset={}, features=[{}]\n\
                     New:     preset={}, features=[{}]\n\n\
                     Use --overwrite to replace, or 'shell-ai integration update' to regenerate with existing preferences.",
                    path.display(),
                    existing_prefs.preset,
                    existing_features.join(", "),
                    args.preset,
                    new_features.join(", ")
                );
            }
        }

        anyhow::bail!(
            "Integration file already exists: {}\n\
             Use --overwrite to replace.",
            path.display()
        );
    }

    // Create parent directory if needed
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("Failed to create config directory")?;
    }

    // Write file
    fs::write(&path, &content).context("Failed to write integration file")?;

    println!("{} {}", "Created:".green(), path.display());
    print_sourcing_instructions(args.shell, &path);

    Ok(())
}

/// Run the update action.
pub fn run_update(args: IntegrationUpdateArgs) -> Result<()> {
    let shells_to_update: Vec<ShellType> = if let Some(shell) = args.shell {
        vec![shell]
    } else {
        // Find all existing integration files
        ShellType::iter()
            .filter(|s| {
                integration_file_path(*s)
                    .map(|p| p.exists())
                    .unwrap_or(false)
            })
            .collect()
    };

    if shells_to_update.is_empty() {
        println!("No integration files found to update.");
        println!(
            "Run '{}' first.",
            "shell-ai integration generate <shell>".cyan()
        );
        return Ok(());
    }

    for shell in shells_to_update {
        let path = integration_file_path(shell)
            .ok_or_else(|| anyhow::anyhow!("Could not determine integration file path"))?;

        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;

        let prefs = parse_header(&content).map_err(|e| {
            anyhow::anyhow!(
                "Could not parse preferences from {}: {}\nWas this file generated by shell-ai?",
                path.display(),
                e
            )
        })?;

        // Regenerate with same preferences
        let new_content =
            generate_integration_file(prefs.shell, prefs.preset, &prefs.add, &prefs.remove);

        fs::write(&path, &new_content)
            .with_context(|| format!("Failed to write {}", path.display()))?;

        println!("{} {}", "Updated:".green(), path.display());
    }

    Ok(())
}

/// Helper to get feature description.
fn feature_description(feature: Feature) -> &'static str {
    match feature {
        Feature::Completions => "Tab completion for shell-ai commands",
        Feature::Aliases => "?? for suggest, explain for explain (Fish: abbreviations)",
        Feature::Keybinding => "Ctrl+G transform with animated progress indicator",
    }
}

/// Collect installed integrations info.
fn collect_installed_integrations() -> Vec<InstalledIntegration> {
    let mut installed = Vec::new();
    for shell in ShellType::iter() {
        if let Some(path) = integration_file_path(shell) {
            if path.exists() {
                if let Ok(content) = fs::read_to_string(&path) {
                    if let Ok(prefs) = parse_header(&content) {
                        let resolved = resolve_features(prefs.preset, &prefs.add, &prefs.remove);
                        let mut features: Vec<_> =
                            resolved.iter().map(|f| f.to_string()).collect();
                        features.sort();
                        installed.push(InstalledIntegration {
                            shell: shell.to_string(),
                            preset: prefs.preset.to_string(),
                            features,
                            path: path.display().to_string(),
                        });
                    }
                }
            }
        }
    }
    installed
}

/// Run the list action.
pub fn run_list(output_format: OutputFormat) -> Result<()> {
    match output_format {
        OutputFormat::Json => run_list_json(),
        OutputFormat::Human => run_list_human(),
    }
}

fn run_list_json() -> Result<()> {
    let features: Vec<FeatureInfo> = Feature::iter()
        .map(|f| FeatureInfo {
            name: f.to_string(),
            description: feature_description(f).to_string(),
        })
        .collect();

    let presets: Vec<PresetInfo> = Preset::iter()
        .map(|p| {
            let mut preset_features: Vec<_> = p.features().iter().map(|f| f.to_string()).collect();
            preset_features.sort();
            PresetInfo {
                name: p.to_string(),
                features: preset_features,
            }
        })
        .collect();

    let shells: Vec<String> = ShellType::iter().map(|s| s.to_string()).collect();

    let installed = collect_installed_integrations();

    let output = IntegrationListJson {
        features,
        presets,
        shells,
        installed,
    };

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn run_list_human() -> Result<()> {
    const HEADING: &str = "Shell-AI Shell Integration";
    println!("{}", HEADING.bold());
    println!("{}", "=".repeat(HEADING.len()));
    println!();

    // List features
    println!("{}:", "Available Features".cyan());
    for feature in Feature::iter() {
        println!(
            "  {:15} {}",
            feature.to_string().white(),
            feature_description(feature).dimmed()
        );
    }
    println!();

    // List presets
    println!("{}:", "Presets".cyan());
    for preset in Preset::iter() {
        let mut features: Vec<_> = preset.features().iter().map(|f| f.to_string()).collect();
        features.sort();
        println!(
            "  {:15} [{}]",
            preset.to_string().white(),
            features.join(", ").dimmed()
        );
    }
    println!();

    // List supported shells
    println!("{}:", "Supported Shells".cyan());
    for shell in ShellType::iter() {
        println!("  {}", shell.to_string().white());
    }
    println!();

    // List existing integration files
    println!("{}:", "Installed Integrations".cyan());
    let installed = collect_installed_integrations();
    if installed.is_empty() {
        println!("  {}", "(none)".dimmed());
    } else {
        for inst in installed {
            println!("  {} ({})", inst.shell.green(), inst.features.join(", "));
        }
    }

    Ok(())
}

/// Main entry point for the integration subcommand.
pub fn run(args: IntegrationArgs, output_format: OutputFormat) -> Result<()> {
    match args.action {
        IntegrationAction::Generate(gen_args) => run_generate(gen_args),
        IntegrationAction::Update(update_args) => run_update(update_args),
        IntegrationAction::List => run_list(output_format),
    }
}

// =============================================================================
// Shell-specific templates
// =============================================================================

const BASH_ALIASES: &str = r##"
# === Aliases ===
alias '??'='shell-ai suggest --'
alias 'explain'='shell-ai explain --'
"##;

const BASH_KEYBINDING: &str = r##"
# === Keybinding ===
# Ctrl+G: Transform current line into a shell command
_shai_transform() {
    if [[ -n "$READLINE_LINE" ]]; then
        local original="$READLINE_LINE"
        local len=${#original}
        local tmpfile=$(mktemp)
        local had_monitor=0
        local pid
        local spinner=(⠋ ⠙ ⠹ ⠸ ⠼ ⠴ ⠦ ⠧ ⠇ ⠏)
        [[ $- == *m* ]] && had_monitor=1

        set +m
        trap 'kill $pid 2>/dev/null; (( had_monitor )) && set -m; rm -f "$tmpfile"; printf "\r\033[K"; trap - INT TERM; return' INT TERM

        { shell-ai --frontend=noninteractive suggest -- "$original" 2>/dev/null | head -1 > "$tmpfile" & } 2>/dev/null
        pid=$!

        local pos=0
        while kill -0 $pid 2>/dev/null; do
            local highlighted=""
            for ((j=0; j<len; j++)); do
                local dist=$(( j - pos ))
                (( dist < 0 )) && dist=$(( -dist ))
                local wrap_dist=$(( len - dist ))
                (( pos > 2 && wrap_dist < dist )) && dist=$wrap_dist
                if (( dist == 0 )); then
                    highlighted+="\033[1;96m${original:j:1}"
                elif (( dist <= 2 )); then
                    highlighted+="\033[0;36m${original:j:1}"
                else
                    highlighted+="\033[2;36m${original:j:1}"
                fi
            done
            printf '\r\033[K\033[1;36m%s\033[0m %b\033[0m' "${spinner[pos % ${#spinner[@]}]}" "$highlighted"
            sleep 0.08
            pos=$(( (pos + 1) % len ))
        done

        trap - INT TERM
        (( had_monitor )) && set -m
        READLINE_LINE=$(< "$tmpfile")
        READLINE_POINT=${#READLINE_LINE}
        rm -f "$tmpfile"
        printf '\r\033[K'
    fi
}
bind -x '"\C-g": _shai_transform'
"##;

const ZSH_ALIASES: &str = r##"
# === Aliases ===
alias '??'='shell-ai suggest --'
alias 'explain'='shell-ai explain --'
"##;

const ZSH_KEYBINDING: &str = r##"
# === Keybinding ===
# Ctrl+G: Transform current line into a shell command
_shai_transform() {
    if [[ -n "$BUFFER" ]]; then
        local original="$BUFFER"
        local len=${#original}
        local tmpfile=$(mktemp)
        local spinner=(⠋ ⠙ ⠹ ⠸ ⠼ ⠴ ⠦ ⠧ ⠇ ⠏)
        local pid

        setopt LOCAL_OPTIONS NO_NOTIFY NO_MONITOR LOCAL_TRAPS
        trap 'kill $pid 2>/dev/null; rm -f "$tmpfile"; printf "\r\033[K"; zle reset-prompt; return' INT TERM

        (shell-ai --frontend=noninteractive suggest -- "$original" 2>/dev/null | head -1 > "$tmpfile") &!
        pid=$!

        local pos=0
        while kill -0 $pid 2>/dev/null; do
            local highlighted=""
            for ((j=1; j<=len; j++)); do
                local dist=$(( j - 1 - pos ))
                (( dist < 0 )) && dist=$(( -dist ))
                local wrap_dist=$(( len - dist ))
                (( pos > 2 && wrap_dist < dist )) && dist=$wrap_dist
                if (( dist == 0 )); then
                    highlighted+="\033[1;96m${original[j]}"
                elif (( dist <= 2 )); then
                    highlighted+="\033[0;36m${original[j]}"
                else
                    highlighted+="\033[2;36m${original[j]}"
                fi
            done
            printf '\r\033[K\033[1;36m%s\033[0m %b\033[0m' "${spinner[pos % ${#spinner[@]} + 1]}" "$highlighted"
            sleep 0.08
            pos=$(( (pos + 1) % len ))
        done

        BUFFER=$(< "$tmpfile")
        rm -f "$tmpfile"
        printf '\r\033[K'
        zle reset-prompt
        zle end-of-line
    fi
}
zle -N _shai_transform
bindkey '^G' _shai_transform
"##;

const FISH_ALIASES: &str = r##"
# === Abbreviations ===
# Fish uses abbreviations instead of aliases for better integration
abbr -a '??' 'shell-ai suggest --'
abbr -a 'explain' 'shell-ai explain --'
"##;

const FISH_KEYBINDING: &str = r##"
# === Keybinding ===
# Ctrl+G: Transform current line into a shell command
function _shai_transform
    set -l cmd (commandline)
    test -z "$cmd"; and return

    set -g __shai_cmd $cmd
    set -g __shai_tmp (mktemp)
    set -g __shai_pid
    set -g __shai_cancelled 0
    set -l spinner ⠋ ⠙ ⠹ ⠸ ⠼ ⠴ ⠦ ⠧ ⠇ ⠏
    set -l len (string length "$cmd")

    function __shai_cancel --on-event fish_cancel --on-signal INT
        set -g __shai_cancelled 1
        kill $__shai_pid 2>/dev/null
    end

    sh -c 'shell-ai --frontend=noninteractive suggest -- "$1" 2>/dev/null | head -1 > "$2"' _ "$cmd" "$__shai_tmp" &
    set __shai_pid $last_pid

    set -l pos 0
    while kill -0 $__shai_pid 2>/dev/null; and test $__shai_cancelled -eq 0
        set -l highlighted ""
        for j in (seq $len)
            set -l dist (math "abs($j - 1 - $pos)")
            set -l wrap_dist (math "$len - $dist")
            test $pos -gt 2 -a $wrap_dist -lt $dist; and set dist $wrap_dist
            if test $dist -eq 0
                set highlighted "$highlighted"\e"[1;96m"(string sub -s $j -l 1 "$cmd")
            else if test $dist -le 2
                set highlighted "$highlighted"\e"[0;36m"(string sub -s $j -l 1 "$cmd")
            else
                set highlighted "$highlighted"\e"[2;36m"(string sub -s $j -l 1 "$cmd")
            end
        end
        printf '\r\033[K\033[1;36m%s\033[0m %b\033[0m' $spinner[(math "$pos % 10 + 1")] "$highlighted"
        sleep 0.08 &; wait $last_pid; or break
        set pos (math "($pos + 1) % $len")
    end

    functions -e __shai_cancel
    printf '\r\033[K'
    if test $__shai_cancelled -eq 1
        commandline -r $__shai_cmd
    else
        commandline -r (cat $__shai_tmp)
    end
    rm -f $__shai_tmp
    set -e __shai_pid __shai_tmp __shai_cmd __shai_cancelled
    commandline -f repaint
    commandline -f end-of-line
end
bind \cg _shai_transform
"##;

const POWERSHELL_ALIASES: &str = r##"
# === Functions (PowerShell equivalent of aliases) ===
function ?? { shell-ai suggest -- @args }
function explain { shell-ai explain -- @args }
"##;

const POWERSHELL_KEYBINDING: &str = r##"
# === Keybinding ===
# Ctrl+G: Transform current line into a shell command
Set-PSReadLineKeyHandler -Chord 'Ctrl+g' -ScriptBlock {
    $line = $null
    [Microsoft.PowerShell.PSConsoleReadLine]::GetBufferState([ref]$line, [ref]$null)
    if ($line) {
        $len = $line.Length
        $spinner = @('⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏')
        $cancelled = $false

        $job = Start-Job -ScriptBlock {
            param($l)
            shell-ai --frontend=noninteractive suggest -- $l 2>$null | Select-Object -First 1
        } -ArgumentList $line

        $pos = 0
        while ($job.State -eq 'Running') {
            if ([Console]::KeyAvailable) {
                $key = [Console]::ReadKey($true)
                if ($key.Key -eq 'C' -and $key.Modifiers -eq 'Control') {
                    $cancelled = $true
                    break
                }
            }
            $highlighted = ""
            for ($j = 0; $j -lt $len; $j++) {
                $dist = [Math]::Abs($j - $pos)
                $wrapDist = $len - $dist
                if ($pos -gt 2 -and $wrapDist -lt $dist) { $dist = $wrapDist }
                if ($dist -eq 0) {
                    $highlighted += "`e[1;96m$($line[$j])"
                } elseif ($dist -le 2) {
                    $highlighted += "`e[0;36m$($line[$j])"
                } else {
                    $highlighted += "`e[2;36m$($line[$j])"
                }
            }
            $spin = $spinner[$pos % $spinner.Length]
            [Console]::Write("`r`e[K`e[1;36m$spin`e[0m $highlighted`e[0m")
            Start-Sleep -Milliseconds 80
            $pos = ($pos + 1) % $len
        }

        if ($cancelled) {
            Stop-Job $job
            Remove-Job $job
            [Console]::Write("`r`e[K")
            [Microsoft.PowerShell.PSConsoleReadLine]::InvokePrompt()
        } else {
            $result = Receive-Job $job
            Remove-Job $job
            [Console]::Write("`r`e[K")
            [Microsoft.PowerShell.PSConsoleReadLine]::Replace(0, $line.Length, $result)
            [Microsoft.PowerShell.PSConsoleReadLine]::InvokePrompt()
        }
    }
}
"##;
