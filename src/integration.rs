//! Shell integration generation
//!
//! Generates integration scripts with configurable features:
//! - completions: Tab completion for CLI commands
//! - aliases: ?? for suggest, explain for explain
//! - keybinding: Ctrl+G inline transform with progress indicator

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
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
    /// Generate a new integration script
    Generate(IntegrationGenerateArgs),
    /// Update existing integration script(s) using stored preferences
    Update(IntegrationUpdateArgs),
    /// Show available features, presets, and installed integrations
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

    /// Add feature(s) on top of preset. Can be specified multiple times
    #[arg(long = "add", short = 'a', value_name = "FEATURE")]
    pub add_features: Vec<Feature>,

    /// Remove feature(s) from preset. Can be specified multiple times
    #[arg(long = "remove", short = 'r', value_name = "FEATURE")]
    pub remove_features: Vec<Feature>,

    /// Print to stdout instead of writing to file
    #[arg(long)]
    pub stdout: bool,

    /// Overwrite existing file without confirmation
    #[arg(long, short = 'y')]
    pub overwrite: bool,
}

#[derive(Parser, Debug)]
pub struct IntegrationUpdateArgs {
    /// Target shell. If omitted, updates all existing integration files
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
    /// Tab completion for this program's subcommands
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
    bin: &str,
    shell: ShellType,
    preset: Preset,
    add: &[Feature],
    remove: &[Feature],
) -> String {
    let version = env!("CARGO_PKG_VERSION");
    let modifiers = format_modifiers(add, remove);

    format!(
        r#"# {bin} integration
# Generated by {bin} v{version}
#
# DO NOT EDIT THIS FILE MANUALLY
# Regenerate with: {bin} integration update {shell}
#
# @shell: {shell}
# @preset: {preset}
# @modifiers: {modifiers}
#
"#,
        bin = bin,
        version = version,
        shell = shell.to_string(),
        preset = preset,
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
    let first_line = content.lines().next().unwrap_or("");
    if !(first_line.starts_with("# ") && first_line.ends_with(" integration")) {
        return Err("Not a recognized integration file".to_string());
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
fn generate_completions(bin: &str, shell: ShellType) -> String {
    let mut cmd = Cli::command();
    let clap_shell = match shell {
        ShellType::Bash => ClapShell::Bash,
        ShellType::Zsh => ClapShell::Zsh,
        ShellType::Fish => ClapShell::Fish,
        ShellType::PowerShell => ClapShell::PowerShell,
    };

    let mut buf = Vec::new();
    generate(clap_shell, &mut cmd, bin, &mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

/// Generate the full integration file content.
fn generate_integration_file(
    bin: &str,
    shell: ShellType,
    preset: Preset,
    add: &[Feature],
    remove: &[Feature],
) -> String {
    let features = resolve_features(preset, add, remove);
    let mut output = generate_header(bin, shell, preset, add, remove);

    let (aliases, keybinding) = match shell {
        ShellType::Bash => (BASH_ALIASES, BASH_KEYBINDING),
        ShellType::Zsh => (ZSH_ALIASES, ZSH_KEYBINDING),
        ShellType::Fish => (FISH_ALIASES, FISH_KEYBINDING),
        ShellType::PowerShell => (POWERSHELL_ALIASES, POWERSHELL_KEYBINDING),
    };

    if features.contains(&Feature::Completions) {
        output.push_str("\n# === Completions ===\n");
        output.push_str(&generate_completions(bin, shell));
    }
    if features.contains(&Feature::Aliases) {
        output.push_str(&aliases.replace("{bin}", bin));
    }
    if features.contains(&Feature::Keybinding) {
        output.push_str(&keybinding.replace("{bin}", bin));
    }

    output
}

/// Replace home directory with $HOME for portable paths.
fn path_with_home_var(path: &Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(relative) = path.strip_prefix(&home) {
            return format!("$HOME/{}", relative.display());
        }
    }
    path.display().to_string()
}

/// Print sourcing instructions for the user.
fn print_sourcing_instructions(shell: ShellType, path: &Path) {
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
pub fn run_generate(bin: &str, args: IntegrationGenerateArgs) -> Result<()> {
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
        bin,
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
                     Use --overwrite to replace, or '{} integration update' to regenerate with existing preferences.",
                    path.display(),
                    existing_prefs.preset,
                    existing_features.join(", "),
                    args.preset,
                    new_features.join(", "),
                    bin
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
pub fn run_update(bin: &str, args: IntegrationUpdateArgs) -> Result<()> {
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
            format!("{} integration generate <shell>", bin).cyan()
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
                "Could not parse preferences from {}: {}\nWas this file generated by {}?",
                path.display(),
                e,
                bin
            )
        })?;

        // Regenerate with same preferences
        let new_content =
            generate_integration_file(bin, prefs.shell, prefs.preset, &prefs.add, &prefs.remove);

        fs::write(&path, &new_content)
            .with_context(|| format!("Failed to write {}", path.display()))?;

        println!("{} {}", "Updated:".green(), path.display());
    }

    Ok(())
}

/// Helper to get feature description.
fn feature_description(bin: &str, feature: Feature) -> String {
    match feature {
        Feature::Completions => format!("Tab completion for {} commands", bin),
        Feature::Aliases => "?? for suggest, explain for explain (Fish: abbreviations)".to_string(),
        Feature::Keybinding => "Ctrl+G transform with animated progress indicator".to_string(),
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
pub fn run_list(bin: &str, output_format: OutputFormat) -> Result<()> {
    match output_format {
        OutputFormat::Json => run_list_json(bin),
        OutputFormat::Human => run_list_human(bin),
    }
}

fn run_list_json(bin: &str) -> Result<()> {
    let features: Vec<FeatureInfo> = Feature::iter()
        .map(|f| FeatureInfo {
            name: f.to_string(),
            description: feature_description(bin, f),
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

fn run_list_human(bin: &str) -> Result<()> {
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
            feature_description(bin, feature).dimmed()
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
pub fn run(bin: &str, args: IntegrationArgs, output_format: OutputFormat) -> Result<()> {
    match args.action {
        IntegrationAction::Generate(gen_args) => run_generate(bin, gen_args),
        IntegrationAction::Update(update_args) => run_update(bin, update_args),
        IntegrationAction::List => run_list(bin, output_format),
    }
}

// =============================================================================
// Shell-specific templates
// =============================================================================

const BASH_ALIASES: &str = r##"
# === Aliases ===
alias '??'='{bin} suggest --'
alias 'explain'='{bin} explain --'
"##;

const BASH_KEYBINDING: &str = r##"
# === Keybinding ===
# Ctrl+G: Transform current line into a shell command
_shai_transform() {
    if [[ -n "$READLINE_LINE" ]]; then
        local original="$READLINE_LINE"
        local tmpfile=$(mktemp)
        local had_monitor=0
        local pid
        [[ $- == *m* ]] && had_monitor=1

        set +m
        trap 'kill $pid 2>/dev/null; (( had_monitor )) && set -m; rm -f "$tmpfile"; printf "%s" "$_shai_cleanup"; trap - INT TERM; return' INT TERM

        { {bin} suggest --frontend=noninteractive -- "$original" 2>/dev/null > "$tmpfile" & } 2>/dev/null
        pid=$!

        local prev_cols
        read -r _ prev_cols < <(stty size </dev/tty 2>/dev/null) || prev_cols=80
        eval "$({bin} _shimmer --shell=bash --cols=$prev_cols -- "$original")"
        printf '%s' "$_shai_init"
        local idx=0
        while kill -0 $pid 2>/dev/null; do
            sleep "$_shai_interval"
            local cur_cols
            read -r _ cur_cols < <(stty size </dev/tty 2>/dev/null) || cur_cols=$prev_cols
            if (( cur_cols != prev_cols )); then
                local extra=$(( (prev_cols + cur_cols - 1) / cur_cols - 1 ))
                (( extra > 0 )) && printf '\033[%dA' "$extra"
                printf '\r\033[J'
                eval "$({bin} _shimmer --shell=bash --cols=$cur_cols -- "$original")"
                printf '%s' "$_shai_init"
                idx=0
                prev_cols=$cur_cols
            else
                idx=$(( (idx + 1) % _shai_n ))
                printf '%s' "${_shai_frames[$idx]}"
            fi
        done

        trap - INT TERM
        (( had_monitor )) && set -m
        READLINE_LINE=$(cat "$tmpfile")
        READLINE_POINT=${#READLINE_LINE}
        rm -f "$tmpfile"
        printf '%s' "$_shai_cleanup"
    fi
}
bind -x '"\C-g": _shai_transform'
"##;

const ZSH_ALIASES: &str = r##"
# === Aliases ===
alias '??'='{bin} suggest --'
alias 'explain'='{bin} explain --'
"##;

const ZSH_KEYBINDING: &str = r##"
# === Keybinding ===
# Ctrl+G: Transform current line into a shell command
_shai_transform() {
    if [[ -n "$BUFFER" ]]; then
        local original="$BUFFER"
        local tmpfile=$(mktemp)
        local pid

        setopt LOCAL_OPTIONS NO_NOTIFY NO_MONITOR LOCAL_TRAPS
        trap 'kill $pid 2>/dev/null; rm -f "$tmpfile"; printf "%s" "$_shai_cleanup"; zle reset-prompt; return' INT TERM

        ({bin} suggest --frontend=noninteractive -- "$original" 2>/dev/null > "$tmpfile") &!
        pid=$!

        local prev_cols=${COLUMNS:-80}
        local _shai_resized=0
        eval "$({bin} _shimmer --shell=zsh --cols=$prev_cols -- "$original")"
        printf '%s' "$_shai_init"
        local idx=0
        trap '_shai_resized=1' WINCH
        while kill -0 $pid 2>/dev/null; do
            if (( _shai_resized )); then
                _shai_resized=0
                local new_cols=${COLUMNS:-80}
                local extra=$(( (prev_cols + new_cols - 1) / new_cols - 1 ))
                (( extra > 0 )) && printf '\033[%dA' "$extra"
                printf '\r\033[J'
                eval "$({bin} _shimmer --shell=zsh --cols=$new_cols -- "$original")"
                printf '%s' "$_shai_init"
                idx=0
                prev_cols=$new_cols
            fi
            sleep "$_shai_interval"
            idx=$(( (idx + 1) % _shai_n ))
            printf '%s' "${_shai_frames[$idx]}"
        done

        BUFFER=$(< "$tmpfile")
        rm -f "$tmpfile"
        printf '%s' "$_shai_cleanup"
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
abbr -a '??' '{bin} suggest --'
abbr -a 'explain' '{bin} explain --'
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
    set -g __shai_resized 0
    set -g __shai_prev_cols $COLUMNS

    function __shai_cancel --on-event fish_cancel --on-signal INT
        set -g __shai_cancelled 1
        kill $__shai_pid 2>/dev/null
    end

    function __shai_on_winch --on-signal WINCH
        set -g __shai_resized 1
    end

    sh -c '{bin} suggest --frontend=noninteractive -- "$1" 2>/dev/null > "$2"' _ "$cmd" "$__shai_tmp" &
    set __shai_pid $last_pid

    {bin} _shimmer --shell=fish --cols=$__shai_prev_cols -- "$cmd" | source
    printf '%s' "$_shai_init"
    set -l idx 1
    while kill -0 $__shai_pid 2>/dev/null; and test $__shai_cancelled -eq 0
        if test $__shai_resized -eq 1
            set -g __shai_resized 0
            set -l new_cols $COLUMNS
            set -l extra (math "ceil($__shai_prev_cols / $new_cols) - 1")
            test $extra -gt 0; and printf '\033[%dA' $extra
            printf '\r\033[J'
            {bin} _shimmer --shell=fish --cols=$new_cols -- "$cmd" | source
            printf '%s' "$_shai_init"
            set idx 1
            set -g __shai_prev_cols $new_cols
        end
        sleep $_shai_interval &; wait $last_pid; or break
        set idx (math "($idx % $_shai_n) + 1")
        printf '%s' "$_shai_frames[$idx]"
    end

    functions -e __shai_cancel __shai_on_winch
    printf '%s' "$_shai_cleanup"
    if test $__shai_cancelled -eq 1
        commandline -r $__shai_cmd
    else
        commandline -r (cat $__shai_tmp)
    end
    rm -f $__shai_tmp
    set -e __shai_pid __shai_tmp __shai_cmd __shai_cancelled __shai_resized __shai_prev_cols
    commandline -f repaint
    commandline -f end-of-line
end
bind \cg _shai_transform
"##;

const POWERSHELL_ALIASES: &str = r##"
# === Functions (PowerShell equivalent of aliases) ===
function ?? { {bin} suggest -- @args }
function explain { {bin} explain -- @args }
"##;

const POWERSHELL_KEYBINDING: &str = r##"
# === Keybinding ===
# Ctrl+G: Transform current line into a shell command
Set-PSReadLineKeyHandler -Chord 'Ctrl+g' -ScriptBlock {
    $line = $null
    [Microsoft.PowerShell.PSConsoleReadLine]::GetBufferState([ref]$line, [ref]$null)
    if ($line) {
        $cancelled = $false
        $prevCols = [Console]::WindowWidth

        $job = Start-Job -ScriptBlock {
            param($l)
            {bin} suggest --frontend=noninteractive -- $l 2>$null
        } -ArgumentList $line

        Invoke-Expression (({bin} _shimmer --shell=powershell --cols=$prevCols -- $line) -join "`n")
        [Console]::Write($_shai_init)
        $idx = 0
        while ($job.State -eq 'Running') {
            if ([Console]::KeyAvailable) {
                $key = [Console]::ReadKey($true)
                if ($key.Key -eq 'C' -and $key.Modifiers -eq 'Control') {
                    $cancelled = $true
                    break
                }
            }
            $newCols = [Console]::WindowWidth
            if ($newCols -ne $prevCols) {
                $extra = [Math]::Ceiling($prevCols / $newCols) - 1
                if ($extra -gt 0) { [Console]::Write("`e[$($extra)A") }
                [Console]::Write("`r`e[J")
                Invoke-Expression (({bin} _shimmer --shell=powershell --cols=$newCols -- $line) -join "`n")
                [Console]::Write($_shai_init)
                $idx = 0
                $prevCols = $newCols
            }
            Start-Sleep -Milliseconds ([int]($_shai_interval * 1000))
            $idx = ($idx + 1) % $_shai_n
            [Console]::Write($_shai_frames[$idx])
        }

        if ($cancelled) {
            Stop-Job $job
            Remove-Job $job
            [Console]::Write($_shai_cleanup)
            [Microsoft.PowerShell.PSConsoleReadLine]::InvokePrompt()
        } else {
            $result = (Receive-Job $job) -join "`n"
            Remove-Job $job
            [Console]::Write($_shai_cleanup)
            [Microsoft.PowerShell.PSConsoleReadLine]::Replace(0, $line.Length, $result)
            [Microsoft.PowerShell.PSConsoleReadLine]::InvokePrompt()
        }
    }
}
"##;
