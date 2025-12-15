use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;

use colored::Colorize;
use serde::{Deserialize, Deserializer, Serialize};
use strum::{Display, EnumIter, EnumString, IntoEnumIterator};

// ============================================================================
// Flexible Deserializers (accept both native types and strings)
// ============================================================================

/// Generic flexible deserializer for optional values that can be either
/// the native type or a string representation.
fn deserialize_flexible<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: FromStr + for<'a> Deserialize<'a>,
    T::Err: fmt::Display,
{
    use serde::de::Error;

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Flexible<T> {
        Native(T),
        Str(String),
    }

    let opt: Option<Flexible<T>> = Option::deserialize(deserializer)?;
    match opt {
        None => Ok(None),
        Some(Flexible::Native(n)) => Ok(Some(n)),
        Some(Flexible::Str(s)) => s
            .parse::<T>()
            .map(Some)
            .map_err(|e| D::Error::custom(format!("invalid value \"{}\": {}", s, e))),
    }
}

/// Source of a configuration value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigSource {
    Default,
    TomlFile,
    JsonFile,
    Environment,
    Cli,
}

impl fmt::Display for ConfigSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigSource::Default => write!(f, "default"),
            ConfigSource::TomlFile => write!(f, "toml"),
            ConfigSource::JsonFile => write!(f, "json"),
            ConfigSource::Environment => write!(f, "env"),
            ConfigSource::Cli => write!(f, "cli"),
        }
    }
}

/// A configuration value with its source tracked.
#[derive(Debug, Clone)]
pub struct ConfigValue<T> {
    pub value: T,
    pub source: ConfigSource,
}

impl<T> ConfigValue<T> {
    pub fn new(value: T, source: ConfigSource) -> Self {
        Self { value, source }
    }
}

/// Frontend interaction mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Display, EnumString, EnumIter, Deserialize, Serialize)]
#[strum(serialize_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum Frontend {
    #[default]
    Dialog,
    Readline,
    Noninteractive,
}

/// Output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Display, EnumString, EnumIter, Deserialize, Serialize)]
#[strum(serialize_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    #[default]
    Human,
    Json,
}

/// Supported providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Display, EnumString, EnumIter, Deserialize, Serialize)]
#[strum(serialize_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    #[serde(alias = "openai")]
    OpenAI,
    #[serde(alias = "groq")]
    Groq,
    #[serde(alias = "azure")]
    Azure,
    #[serde(alias = "ollama")]
    Ollama,
    #[serde(alias = "mistral")]
    Mistral,
}

/// Debug/logging level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Display, EnumString, EnumIter, Deserialize, Serialize, clap::ValueEnum)]
#[strum(serialize_all = "lowercase")]
#[serde(rename_all = "lowercase")]
#[clap(rename_all = "lowercase")]
pub enum DebugLevel {
    /// Error level logging only
    Error,
    /// Warning and error level logging
    Warn,
    /// Info, warning, and error level logging
    Info,
    /// Debug level logging
    Debug,
    /// Trace level logging (very verbose)
    Trace,
}

impl DebugLevel {
    /// Convert to log::LevelFilter
    pub fn to_level_filter(self) -> log::LevelFilter {
        match self {
            DebugLevel::Error => log::LevelFilter::Error,
            DebugLevel::Warn => log::LevelFilter::Warn,
            DebugLevel::Info => log::LevelFilter::Info,
            DebugLevel::Debug => log::LevelFilter::Debug,
            DebugLevel::Trace => log::LevelFilter::Trace,
        }
    }
}

// ============================================================================
// Environment Variable Names (Single Source of Truth)
// ============================================================================

/// Environment variable names used by Shell-AI.
pub mod env {
    // Global settings
    pub const SHAI_API_PROVIDER: &str = "SHAI_API_PROVIDER";
    pub const SHAI_PROVIDER: &str = "SHAI_PROVIDER"; // Alias
    pub const SHAI_MODEL: &str = "SHAI_MODEL";
    pub const SHAI_TEMPERATURE: &str = "SHAI_TEMPERATURE";
    pub const SHAI_SUGGESTION_COUNT: &str = "SHAI_SUGGESTION_COUNT";
    pub const SHAI_SKIP_CONFIRM: &str = "SHAI_SKIP_CONFIRM"; // Legacy, implies noninteractive
    pub const SHAI_FRONTEND: &str = "SHAI_FRONTEND";
    pub const SHAI_OUTPUT_FORMAT: &str = "SHAI_OUTPUT_FORMAT";
    pub const SHAI_MAX_REFERENCE_CHARS: &str = "SHAI_MAX_REFERENCE_CHARS";
    pub const SHAI_MAX_TOKENS: &str = "SHAI_MAX_TOKENS";
    pub const SHAI_DEBUG: &str = "SHAI_DEBUG";

    // OpenAI provider
    pub const OPENAI_API_KEY: &str = "OPENAI_API_KEY";
    pub const OPENAI_API_BASE: &str = "OPENAI_API_BASE";
    pub const OPENAI_MODEL: &str = "OPENAI_MODEL";
    pub const OPENAI_ORGANIZATION: &str = "OPENAI_ORGANIZATION";
    pub const OPENAI_MAX_TOKENS: &str = "OPENAI_MAX_TOKENS";
    pub const OPENAI_API_VERSION: &str = "OPENAI_API_VERSION"; // Also used by Azure

    // Groq provider
    pub const GROQ_API_KEY: &str = "GROQ_API_KEY";
    pub const GROQ_MODEL: &str = "GROQ_MODEL";
    pub const GROQ_MAX_TOKENS: &str = "GROQ_MAX_TOKENS";

    // Azure provider
    pub const AZURE_API_KEY: &str = "AZURE_API_KEY";
    pub const AZURE_API_BASE: &str = "AZURE_API_BASE";
    pub const AZURE_DEPLOYMENT_NAME: &str = "AZURE_DEPLOYMENT_NAME";
    pub const AZURE_MAX_TOKENS: &str = "AZURE_MAX_TOKENS";

    // Ollama provider
    pub const OLLAMA_API_BASE: &str = "OLLAMA_API_BASE";
    pub const OLLAMA_MODEL: &str = "OLLAMA_MODEL";
    pub const OLLAMA_MAX_TOKENS: &str = "OLLAMA_MAX_TOKENS";

    // Mistral provider
    pub const MISTRAL_API_KEY: &str = "MISTRAL_API_KEY";
    pub const MISTRAL_API_BASE: &str = "MISTRAL_API_BASE";
    pub const MISTRAL_MODEL: &str = "MISTRAL_MODEL";
    pub const MISTRAL_MAX_TOKENS: &str = "MISTRAL_MAX_TOKENS";
}

// ============================================================================
// Configuration Metadata (Single Source of Truth)
// ============================================================================

/// Display sections for grouping fields in config output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Section {
    Provider,
    Ui,
    Suggest,
    Explain,
    ProviderSpecific,
}

impl Section {
    pub fn title(&self) -> &'static str {
        match self {
            Section::Provider => "Provider Settings",
            Section::Ui => "UI Settings",
            Section::Suggest => "Suggest Settings",
            Section::Explain => "Explain Settings",
            Section::ProviderSpecific => "",
        }
    }
}

/// Metadata for a configuration field.
#[derive(Debug, Clone, Copy)]
pub struct FieldMeta {
    pub name: &'static str,
    pub env_var: Option<&'static str>,
    pub env_aliases: &'static [&'static str],
    pub description: &'static str,
    pub default: Option<&'static str>,
    pub required: bool,
    pub section: Section,
    pub deprecated: bool,
    pub sensitive: bool,
    pub virtual_field: bool, // true for fields not in TomlConfig (e.g., skip_confirm)
}

impl FieldMeta {
    /// Get the default value as a serde_json::Value.
    pub fn default_json_value(&self) -> Option<serde_json::Value> {
        self.default.map(|s| {
            // Try parsing as number first, then bool, then string
            if let Ok(n) = s.parse::<i64>() {
                serde_json::json!(n)
            } else if let Ok(f) = s.parse::<f64>() {
                serde_json::json!(f)
            } else if s == "true" {
                serde_json::Value::Bool(true)
            } else if s == "false" {
                serde_json::Value::Bool(false)
            } else {
                serde_json::json!(s)
            }
        })
    }
}

/// Common field definition (no env_var/default - those are provider-specific)
#[derive(Debug, Clone, Copy)]
pub struct CommonFieldMeta {
    pub name: &'static str,
    pub description: &'static str,
    pub required: bool,
    pub sensitive: bool,
}

/// Per-provider override for common field env_var, default, and required
#[derive(Debug, Clone, Copy)]
pub struct FieldOverride {
    pub name: &'static str,
    pub env_var: Option<&'static str>,
    pub default: Option<&'static str>,
    pub required: Option<bool>,
}

/// Metadata for a provider.
#[derive(Debug, Clone, Copy)]
pub struct ProviderMeta {
    pub name: &'static str,
    pub display_name: &'static str,
    pub description: &'static str,
    pub field_overrides: &'static [FieldOverride],
    pub extra_fields: &'static [FieldMeta],
    pub skip_common: &'static [&'static str],
}

impl ProviderMeta {
    /// Get resolved FieldMeta for a common field with provider-specific overrides
    pub fn resolve_common_field(&self, common: &CommonFieldMeta) -> FieldMeta {
        let override_opt = self.field_overrides.iter().find(|o| o.name == common.name);
        // Use override's required if present, otherwise common's required (unless skipped)
        let required = override_opt
            .and_then(|o| o.required)
            .unwrap_or(common.required && !self.skip_common.contains(&common.name));
        FieldMeta {
            name: common.name,
            description: common.description,
            required,
            sensitive: common.sensitive,
            env_var: override_opt.and_then(|o| o.env_var),
            env_aliases: &[],
            default: override_opt.and_then(|o| o.default),
            section: Section::ProviderSpecific,
            deprecated: false,
            virtual_field: false,
        }
    }

    /// Iterate over all fields (common + extra)
    pub fn all_fields(&self) -> impl Iterator<Item = FieldMeta> + '_ {
        COMMON_PROVIDER_FIELDS
            .iter()
            .filter(|f| !self.skip_common.contains(&f.name))
            .map(|f| self.resolve_common_field(f))
            .chain(self.extra_fields.iter().copied())
    }

    /// Get a single resolved field by name.
    pub fn resolved_field(&self, name: &str) -> Option<FieldMeta> {
        // Check common fields first
        if let Some(common) = COMMON_PROVIDER_FIELDS.iter().find(|c| c.name == name) {
            if !self.skip_common.contains(&common.name) {
                return Some(self.resolve_common_field(common));
            }
        }
        // Then check extra fields
        self.extra_fields.iter().find(|f| f.name == name).copied()
    }
}

/// Common provider fields shared across all providers.
pub const COMMON_PROVIDER_FIELDS: &[CommonFieldMeta] = &[
    CommonFieldMeta {
        name: "api_key",
        description: "API key for authentication",
        required: true,
        sensitive: true,
    },
    CommonFieldMeta {
        name: "api_base",
        description: "API base URL",
        required: false,
        sensitive: false,
    },
    CommonFieldMeta {
        name: "model",
        description: "Model to use",
        required: false,
        sensitive: false,
    },
    CommonFieldMeta {
        name: "max_tokens",
        description: "Max tokens for AI completion",
        required: false,
        sensitive: false,
    },
];

/// Global settings metadata.
pub const GLOBAL_SETTINGS_METADATA: &[FieldMeta] = &[
    FieldMeta {
        name: "provider",
        env_var: Some(env::SHAI_API_PROVIDER),
        env_aliases: &[env::SHAI_PROVIDER],
        description: "Provider to use",
        default: None,
        required: true,
        section: Section::Provider,
        deprecated: false,
        sensitive: false,
        virtual_field: false,
    },
    FieldMeta {
        name: "model",
        env_var: Some(env::SHAI_MODEL),
        env_aliases: &[],
        description: "Override model (takes precedence over provider-specific)",
        default: None,
        required: false,
        section: Section::Provider,
        deprecated: false,
        sensitive: false,
        virtual_field: false,
    },
    FieldMeta {
        name: "temperature",
        env_var: Some(env::SHAI_TEMPERATURE),
        env_aliases: &[],
        description: "Sampling temperature (0.0 = deterministic, 1.0 = creative)",
        default: Some("0.05"),
        required: false,
        section: Section::Provider,
        deprecated: false,
        sensitive: false,
        virtual_field: false,
    },
    FieldMeta {
        name: "suggestion_count",
        env_var: Some(env::SHAI_SUGGESTION_COUNT),
        env_aliases: &[],
        description: "Number of suggestions to generate",
        default: Some("3"),
        required: false,
        section: Section::Suggest,
        deprecated: false,
        sensitive: false,
        virtual_field: false,
    },
    FieldMeta {
        name: "skip_confirm",
        env_var: Some(env::SHAI_SKIP_CONFIRM),
        env_aliases: &[],
        description: "Legacy: skip confirmation (implies frontend=noninteractive)",
        default: Some("false"),
        required: false,
        section: Section::Ui,
        deprecated: true,
        sensitive: false,
        virtual_field: true, // Not in TomlConfig
    },
    FieldMeta {
        name: "frontend",
        env_var: Some(env::SHAI_FRONTEND),
        env_aliases: &[],
        description: "UI mode: dialog, readline, or noninteractive",
        default: Some("dialog"),
        required: false,
        section: Section::Ui,
        deprecated: false,
        sensitive: false,
        virtual_field: false,
    },
    FieldMeta {
        name: "output_format",
        env_var: Some(env::SHAI_OUTPUT_FORMAT),
        env_aliases: &[],
        description: "Output format: human or json",
        default: Some("human"),
        required: false,
        section: Section::Ui,
        deprecated: false,
        sensitive: false,
        virtual_field: false,
    },
    FieldMeta {
        name: "max_reference_chars",
        env_var: Some(env::SHAI_MAX_REFERENCE_CHARS),
        env_aliases: &[],
        description: "Max characters for man page references in explain",
        default: Some("262144"),
        required: false,
        section: Section::Explain,
        deprecated: false,
        sensitive: false,
        virtual_field: false,
    },
    FieldMeta {
        name: "max_tokens",
        env_var: Some(env::SHAI_MAX_TOKENS),
        env_aliases: &[],
        description: "Max tokens for an AI completion (optional, API auto-calculates when omitted)",
        default: None,
        required: false,
        section: Section::Provider,
        deprecated: false,
        sensitive: false,
        virtual_field: false,
    },
    FieldMeta {
        name: "debug",
        env_var: Some(env::SHAI_DEBUG),
        env_aliases: &[],
        description: "Debug log level",
        default: None,
        required: false,
        section: Section::Ui,
        deprecated: false,
        sensitive: false,
        virtual_field: false,
    },
];

/// Provider-specific metadata.
pub const PROVIDER_METADATA: &[ProviderMeta] = &[
    ProviderMeta {
        name: "openai",
        display_name: "OpenAI",
        description: "OpenAI API (GPT-3.5, GPT-4, etc.)",
        field_overrides: &[
            FieldOverride { name: "api_key", env_var: Some(env::OPENAI_API_KEY), default: None, required: None },
            FieldOverride { name: "api_base", env_var: Some(env::OPENAI_API_BASE), default: Some("https://api.openai.com"), required: None },
            FieldOverride { name: "model", env_var: Some(env::OPENAI_MODEL), default: Some("gpt-5"), required: None },
            FieldOverride { name: "max_tokens", env_var: Some(env::OPENAI_MAX_TOKENS), default: None, required: None },
        ],
        extra_fields: &[
            FieldMeta {
                name: "organization",
                env_var: Some(env::OPENAI_ORGANIZATION),
                env_aliases: &[],
                description: "Organization ID for API billing (for multi-org accounts)",
                default: None,
                required: false,
                section: Section::ProviderSpecific,
                deprecated: false,
                sensitive: false,
                virtual_field: false,
            },
        ],
        skip_common: &[],
    },
    ProviderMeta {
        name: "groq",
        display_name: "Groq",
        description: "Groq API (fast inference)",
        field_overrides: &[
            FieldOverride { name: "api_key", env_var: Some(env::GROQ_API_KEY), default: None, required: None },
            FieldOverride { name: "api_base", env_var: None, default: Some("https://api.groq.com/openai"), required: None },
            FieldOverride { name: "model", env_var: Some(env::GROQ_MODEL), default: Some("openai/gpt-oss-120b"), required: None },
            FieldOverride { name: "max_tokens", env_var: Some(env::GROQ_MAX_TOKENS), default: None, required: None },
        ],
        extra_fields: &[],
        skip_common: &[],
    },
    ProviderMeta {
        name: "azure",
        display_name: "Azure OpenAI",
        description: "Azure OpenAI Service",
        field_overrides: &[
            FieldOverride { name: "api_key", env_var: Some(env::AZURE_API_KEY), default: None, required: None },
            FieldOverride { name: "api_base", env_var: Some(env::AZURE_API_BASE), default: None, required: Some(true) },
            FieldOverride { name: "model", env_var: None, default: None, required: None },
            FieldOverride { name: "max_tokens", env_var: Some(env::AZURE_MAX_TOKENS), default: None, required: None },
        ],
        extra_fields: &[
            FieldMeta {
                name: "deployment_name",
                env_var: Some(env::AZURE_DEPLOYMENT_NAME),
                env_aliases: &[],
                description: "Deployment name for your model",
                default: None,
                required: true,
                section: Section::ProviderSpecific,
                deprecated: false,
                sensitive: false,
                virtual_field: false,
            },
            FieldMeta {
                name: "api_version",
                env_var: Some(env::OPENAI_API_VERSION),
                env_aliases: &[],
                description: "Azure API version",
                default: Some("2023-05-15"),
                required: false,
                section: Section::ProviderSpecific,
                deprecated: false,
                sensitive: false,
                virtual_field: false,
            },
        ],
        skip_common: &["model"], // Azure uses deployment_name instead of model
    },
    ProviderMeta {
        name: "ollama",
        display_name: "Ollama",
        description: "Local Ollama instance (no API key required)",
        field_overrides: &[
            FieldOverride { name: "api_key", env_var: None, default: None, required: None },
            FieldOverride { name: "api_base", env_var: Some(env::OLLAMA_API_BASE), default: Some("http://localhost:11434"), required: None },
            FieldOverride { name: "model", env_var: Some(env::OLLAMA_MODEL), default: Some("gpt-oss:120b-cloud"), required: None },
            FieldOverride { name: "max_tokens", env_var: Some(env::OLLAMA_MAX_TOKENS), default: None, required: None },
        ],
        extra_fields: &[],
        skip_common: &["api_key"], // Ollama doesn't require api_key
    },
    ProviderMeta {
        name: "mistral",
        display_name: "Mistral AI",
        description: "Mistral AI API",
        field_overrides: &[
            FieldOverride { name: "api_key", env_var: Some(env::MISTRAL_API_KEY), default: None, required: None },
            FieldOverride { name: "api_base", env_var: Some(env::MISTRAL_API_BASE), default: Some("https://api.mistral.ai"), required: None },
            FieldOverride { name: "model", env_var: Some(env::MISTRAL_MODEL), default: Some("codestral-2508"), required: None },
            FieldOverride { name: "max_tokens", env_var: Some(env::MISTRAL_MAX_TOKENS), default: None, required: None },
        ],
        extra_fields: &[],
        skip_common: &[],
    },
];

impl Provider {
    /// Get metadata for this provider.
    pub fn metadata(&self) -> &'static ProviderMeta {
        let name = self.to_string().to_lowercase();
        PROVIDER_METADATA
            .iter()
            .find(|m| m.name == name)
            .expect("Provider metadata missing - this is a bug")
    }
}

/// Get the environment variable name for a config field path (for error messages).
fn env_var_for_field(field_path: &str) -> Option<&'static str> {
    // Check global settings
    for field in GLOBAL_SETTINGS_METADATA {
        if field.name == field_path {
            return field.env_var;
        }
    }
    // Check provider fields
    for provider in PROVIDER_METADATA {
        for field in provider.all_fields() {
            let path = format!("{}.{}", provider.name, field.name);
            if path == field_path {
                return field.env_var;
            }
        }
    }
    None
}

// ============================================================================
// ConfigBuilder: Merge JSON layers with source tracking
// ============================================================================

/// Builder that tracks both config values and their sources during merge.
struct ConfigBuilder {
    config: serde_json::Value,
    sources: HashMap<String, ConfigSource>,
    /// Tracks which env var was actually used for each config path (for error hints)
    env_vars_used: HashMap<String, String>,
}

impl ConfigBuilder {
    fn new() -> Self {
        Self {
            config: serde_json::Value::Object(serde_json::Map::new()),
            sources: HashMap::new(),
            env_vars_used: HashMap::new(),
        }
    }

    /// Record which env var was used for a config path.
    fn record_env_var(&mut self, config_path: &str, env_var: &str) {
        self.env_vars_used.insert(config_path.to_string(), env_var.to_string());
    }

    /// Get the actual env var used for a config path.
    fn get_env_var_used(&self, path: &str) -> Option<&str> {
        self.env_vars_used.get(path).map(|s| s.as_str())
    }

    fn merge_layer(&mut self, layer: &serde_json::Value, source: ConfigSource) {
        self.merge_recursive(layer, source, String::new());
    }

    fn merge_recursive(&mut self, layer: &serde_json::Value, source: ConfigSource, path: String) {
        if let serde_json::Value::Object(obj) = layer {
            for (key, value) in obj {
                let full_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{}.{}", path, key)
                };

                if value.is_object() {
                    if !value.as_object().unwrap().is_empty() {
                        // Non-empty object: recurse into it
                        self.merge_recursive(value, source, full_path);
                    }
                    // Empty object: skip (don't overwrite existing values with {})
                } else if !value.is_null() {
                    self.sources.insert(full_path.clone(), source);
                    Self::set_nested_value(
                        self.config.as_object_mut().unwrap(),
                        &full_path,
                        value.clone(),
                    );
                }
            }
        }
    }

    fn set_nested_value(
        obj: &mut serde_json::Map<String, serde_json::Value>,
        path: &str,
        value: serde_json::Value,
    ) {
        let parts: Vec<&str> = path.split('.').collect();
        if parts.len() == 1 {
            obj.insert(parts[0].to_string(), value);
        } else {
            let entry = obj
                .entry(parts[0].to_string())
                .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
            if let serde_json::Value::Object(nested) = entry {
                Self::set_nested_value(nested, &parts[1..].join("."), value);
            }
        }
    }

    fn get_source(&self, path: &str) -> ConfigSource {
        self.sources.get(path).copied().unwrap_or(ConfigSource::Default)
    }

    fn into_sources(self) -> HashMap<String, ConfigSource> {
        self.sources
    }
}

/// Format a serde_path_to_error error with source attribution.
fn format_serde_error(
    err: serde_path_to_error::Error<serde_json::Error>,
    builder: &ConfigBuilder,
) -> String {
    let path = err.path().to_string();
    let inner_err = strip_json_location(err.inner().to_string());

    if path.is_empty() {
        inner_err
    } else {
        let source = builder.get_source(&path);
        let source_hint = source_to_hint(source, &path, builder.get_env_var_used(&path));
        format!("{}: {}", source_hint, inner_err)
    }
}

fn strip_json_location(msg: String) -> String {
    if let Some(idx) = msg.rfind(" at line ") {
        msg[..idx].to_string()
    } else {
        msg
    }
}

/// Convert a config source to a hint string for error messages.
/// For environment sources, uses the actual env var name if available.
fn source_to_hint(source: ConfigSource, field_path: &str, actual_env_var: Option<&str>) -> String {
    match source {
        ConfigSource::Cli => {
            let flag_name = field_path.replace('.', "-");
            format!("--{}", flag_name)
        }
        ConfigSource::Environment => {
            // Use the actual env var name if we recorded it, otherwise fall back to metadata
            actual_env_var
                .map(|s| s.to_string())
                .or_else(|| env_var_for_field(field_path).map(|s| s.to_string()))
                .unwrap_or_else(|| field_path.to_uppercase())
        }
        ConfigSource::JsonFile => "config.json".to_string(),
        ConfigSource::TomlFile => "config.toml".to_string(),
        ConfigSource::Default => "default".to_string(),
    }
}

// ============================================================================
// JSON Conversion Functions
// ============================================================================

/// Generate defaults JSON from metadata (single source of truth).
fn defaults_to_json() -> serde_json::Value {
    let mut obj = serde_json::Map::new();

    // Global settings (skip virtual fields)
    for field in GLOBAL_SETTINGS_METADATA {
        if field.virtual_field {
            continue;
        }
        if let Some(val) = field.default_json_value() {
            obj.insert(field.name.to_string(), val);
        }
    }

    // Provider settings (use resolved fields with overrides)
    for provider in PROVIDER_METADATA {
        let mut pobj = serde_json::Map::new();
        for field in provider.all_fields() {
            if let Some(val) = field.default_json_value() {
                pobj.insert(field.name.to_string(), val);
            }
        }
        if !pobj.is_empty() {
            obj.insert(provider.name.to_string(), serde_json::Value::Object(pobj));
        }
    }

    serde_json::Value::Object(obj)
}

/// Convert environment variables to a JSON object.
/// Primary env vars win over aliases (first one set for a path wins).
/// Records which env var was used in the builder for accurate error hints.
fn env_to_json(builder: &mut ConfigBuilder) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    let mut seen_paths = std::collections::HashSet::new();

    // Process env vars in metadata order (primary before aliases)
    for field in GLOBAL_SETTINGS_METADATA {
        if field.virtual_field {
            continue;
        }
        // Helper to try setting a field from an env var
        let mut try_set_from_env = |env_var: &str| {
            if seen_paths.contains(field.name) {
                return;
            }
            if let Ok(value) = std::env::var(env_var) {
                if !value.is_empty() {
                    obj.insert(field.name.to_string(), serde_json::Value::String(value));
                    seen_paths.insert(field.name.to_string());
                    builder.record_env_var(field.name, env_var);
                }
            }
        };

        // Primary env var first, then aliases
        if let Some(env_var) = field.env_var {
            try_set_from_env(env_var);
        }
        for alias in field.env_aliases {
            try_set_from_env(alias);
        }
    }

    // Provider-specific env vars
    for provider in PROVIDER_METADATA {
        for field in provider.all_fields() {
            if let Some(env_var) = field.env_var {
                let path = format!("{}.{}", provider.name, field.name);
                if !seen_paths.contains(&path) {
                    if let Ok(value) = std::env::var(env_var) {
                        if !value.is_empty() {
                            ConfigBuilder::set_nested_value(
                                &mut obj,
                                &path,
                                serde_json::Value::String(value),
                            );
                            seen_paths.insert(path.clone());
                            builder.record_env_var(&path, env_var);
                        }
                    }
                }
            }
        }
    }

    // Handle legacy SHAI_SKIP_CONFIRM
    if let Ok(v) = std::env::var(env::SHAI_SKIP_CONFIRM) {
        if v.to_lowercase() == "true" {
            if std::env::var(env::SHAI_FRONTEND).is_err() {
                obj.insert(
                    "frontend".to_string(),
                    serde_json::Value::String("noninteractive".to_string()),
                );
                builder.record_env_var("frontend", env::SHAI_SKIP_CONFIRM);
            }
        }
    }

    serde_json::Value::Object(obj)
}

/// CLI overrides to pass to AppConfig::load().
#[derive(Debug, Default, Serialize)]
pub struct CliOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frontend: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debug: Option<DebugLevel>,
}

/// Convert CLI arguments to a JSON object using serde.
fn cli_to_json(overrides: &CliOverrides) -> serde_json::Value {
    serde_json::to_value(overrides).unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()))
}

// ============================================================================
// Provider-specific credentials and settings
// ============================================================================

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProviderCredentials {
    pub api_key: Option<String>,
    pub api_base: Option<String>,
    pub model: Option<String>,
    #[serde(default, deserialize_with = "deserialize_flexible")]
    pub max_tokens: Option<u32>,
    // OpenAI-specific
    pub organization: Option<String>,
    // Azure-specific
    pub deployment_name: Option<String>,
    pub api_version: Option<String>,
}

impl ProviderCredentials {
    /// Get a field value by name as Option<String>.
    pub fn get_field(&self, name: &str) -> Option<String> {
        match name {
            "api_key" => self.api_key.clone(),
            "api_base" => self.api_base.clone(),
            "model" => self.model.clone(),
            "organization" => self.organization.clone(),
            "max_tokens" => self.max_tokens.map(|t| t.to_string()),
            "deployment_name" => self.deployment_name.clone(),
            "api_version" => self.api_version.clone(),
            _ => None,
        }
    }
}

/// Result of validating configuration for a specific provider.
#[derive(Debug)]
pub struct ValidationError {
    pub field: String,
    pub description: String,
    pub hint: String,
}

/// TOML config file structure.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TomlConfig {
    pub provider: Option<Provider>,
    pub model: Option<String>,
    #[serde(default, deserialize_with = "deserialize_flexible")]
    pub temperature: Option<f32>,
    #[serde(default, deserialize_with = "deserialize_flexible")]
    pub suggestion_count: Option<u32>,
    pub frontend: Option<Frontend>,
    pub output_format: Option<OutputFormat>,
    #[serde(default, deserialize_with = "deserialize_flexible")]
    pub max_reference_chars: Option<u32>,
    #[serde(default, deserialize_with = "deserialize_flexible")]
    pub max_tokens: Option<u32>,
    pub debug: Option<DebugLevel>,

    // Provider-specific sections
    pub openai: Option<ProviderCredentials>,
    pub groq: Option<ProviderCredentials>,
    pub azure: Option<ProviderCredentials>,
    pub ollama: Option<ProviderCredentials>,
    pub mistral: Option<ProviderCredentials>,
}

/// Unified application configuration with source tracking.
#[derive(Debug, Clone)]
pub struct AppConfig {
    // Provider settings
    pub provider: ConfigValue<Option<Provider>>,
    pub model: ConfigValue<String>,
    pub temperature: ConfigValue<f32>,

    // UI settings
    pub frontend: ConfigValue<Frontend>,
    pub output_format: ConfigValue<OutputFormat>,

    // Suggest-specific settings
    pub suggestion_count: ConfigValue<u32>,

    // Explain-specific settings
    pub max_reference_chars: ConfigValue<u32>,

    // API request settings
    pub max_tokens: ConfigValue<Option<u32>>,

    // Debug/logging level
    pub debug: ConfigValue<Option<DebugLevel>>,

    // Provider credentials (HashMap instead of individual fields)
    pub providers: HashMap<Provider, ProviderCredentials>,

    // Source tracking for all config paths
    sources: HashMap<String, ConfigSource>,

    // Config file paths for reporting
    pub toml_path: Option<PathBuf>,
    pub json_path: Option<PathBuf>,
}

/// A validated configuration that guarantees provider and credentials exist.
pub struct ValidatedConfig<'a> {
    config: &'a AppConfig,
    pub provider: &'a Provider,
    pub credentials: &'a ProviderCredentials,
}

impl<'a> ValidatedConfig<'a> {
    pub fn app_config(&self) -> &'a AppConfig {
        self.config
    }

    pub fn effective_model(&self) -> String {
        self.config.effective_model()
    }

    pub fn temperature(&self) -> f32 {
        self.config.temperature.value
    }

    pub fn effective_max_tokens(&self) -> Option<u32> {
        self.config.effective_max_tokens()
    }
}

impl AppConfig {
    /// Load configuration with CLI overrides.
    /// Precedence: default -> toml -> json -> env -> cli.
    pub fn load_with_cli(cli: CliOverrides) -> Self {
        let mut builder = ConfigBuilder::new();
        let mut toml_path: Option<PathBuf> = None;
        let mut json_path: Option<PathBuf> = None;

        // Layer 1: Defaults (from metadata)
        builder.merge_layer(&defaults_to_json(), ConfigSource::Default);

        // Layer 2: TOML config
        match load_toml_as_json() {
            TomlJsonLoadResult::Loaded(toml_json, path) => {
                toml_path = Some(path);
                builder.merge_layer(&toml_json, ConfigSource::TomlFile);
            }
            TomlJsonLoadResult::NotFound => {}
            TomlJsonLoadResult::ParseError(path, err) => {
                log::error!(
                    "Failed to parse config file: {}\n\n{}\n\n\
                     Hint: Fix the syntax error above, or delete the file to use defaults.",
                    path.display(),
                    err
                );
                std::process::exit(1);
            }
        }

        // Layer 3: JSON config (legacy)
        match load_json_as_value() {
            JsonValueLoadResult::Loaded(json, path) => {
                json_path = Some(path);
                builder.merge_layer(&json, ConfigSource::JsonFile);
            }
            JsonValueLoadResult::NotFound => {}
            JsonValueLoadResult::ParseError(path, err) => {
                log::error!(
                    "Failed to parse JSON config file: {}\n\n{}\n\n\
                     Hint: Fix the syntax error above, or delete the file to use defaults.",
                    path.display(),
                    err
                );
                std::process::exit(1);
            }
        }

        // Layer 4: Environment variables
        let env_json = env_to_json(&mut builder);
        builder.merge_layer(&env_json, ConfigSource::Environment);

        // Layer 5: CLI arguments
        builder.merge_layer(&cli_to_json(&cli), ConfigSource::Cli);

        // Parse merged JSON into TomlConfig
        let config_json = builder.config.clone();
        let config_str = config_json.to_string();
        let mut deserializer = serde_json::Deserializer::from_str(&config_str);
        let parsed: TomlConfig = match serde_path_to_error::deserialize(&mut deserializer) {
            Ok(p) => p,
            Err(e) => {
                let error_msg = format_serde_error(e, &builder);
                log::error!("Configuration error:\n\n{}", error_msg);
                std::process::exit(1);
            }
        };

        Self::from_parsed(parsed, builder, toml_path, json_path)
    }

    /// Convert parsed TomlConfig to AppConfig with source tracking from builder.
    fn from_parsed(
        parsed: TomlConfig,
        builder: ConfigBuilder,
        toml_path: Option<PathBuf>,
        json_path: Option<PathBuf>,
    ) -> Self {
        // Build providers HashMap
        let mut providers = HashMap::new();
        if let Some(creds) = parsed.openai {
            providers.insert(Provider::OpenAI, creds);
        }
        if let Some(creds) = parsed.groq {
            providers.insert(Provider::Groq, creds);
        }
        if let Some(creds) = parsed.azure {
            providers.insert(Provider::Azure, creds);
        }
        if let Some(creds) = parsed.ollama {
            providers.insert(Provider::Ollama, creds);
        }
        if let Some(creds) = parsed.mistral {
            providers.insert(Provider::Mistral, creds);
        }

        // Ensure all providers have at least default credentials
        for provider in Provider::iter() {
            providers.entry(provider).or_insert_with(ProviderCredentials::default);
        }

        let sources = builder.into_sources();

        Self {
            provider: ConfigValue::new(parsed.provider, sources.get("provider").copied().unwrap_or(ConfigSource::Default)),
            model: ConfigValue::new(
                parsed.model.unwrap_or_default(),
                sources.get("model").copied().unwrap_or(ConfigSource::Default),
            ),
            temperature: ConfigValue::new(
                parsed.temperature.unwrap_or(0.05),
                sources.get("temperature").copied().unwrap_or(ConfigSource::Default),
            ),
            frontend: ConfigValue::new(
                parsed.frontend.unwrap_or(Frontend::Dialog),
                sources.get("frontend").copied().unwrap_or(ConfigSource::Default),
            ),
            output_format: ConfigValue::new(
                parsed.output_format.unwrap_or(OutputFormat::Human),
                sources.get("output_format").copied().unwrap_or(ConfigSource::Default),
            ),
            suggestion_count: ConfigValue::new(
                parsed.suggestion_count.unwrap_or(3),
                sources.get("suggestion_count").copied().unwrap_or(ConfigSource::Default),
            ),
            max_reference_chars: ConfigValue::new(
                parsed.max_reference_chars.unwrap_or(262144),
                sources.get("max_reference_chars").copied().unwrap_or(ConfigSource::Default),
            ),
            max_tokens: ConfigValue::new(
                parsed.max_tokens,
                sources.get("max_tokens").copied().unwrap_or(ConfigSource::Default),
            ),
            debug: ConfigValue::new(
                parsed.debug,
                sources.get("debug").copied().unwrap_or(ConfigSource::Default),
            ),
            providers,
            sources,
            toml_path,
            json_path,
        }
    }

    /// Get credentials for the currently selected provider.
    pub fn current_provider_credentials(&self) -> Option<&ProviderCredentials> {
        self.provider.value.as_ref().and_then(|p| self.providers.get(p))
    }

    /// Get credentials for a specific provider.
    pub fn get_credentials_for(&self, provider: &Provider) -> Option<&ProviderCredentials> {
        self.providers.get(provider)
    }

    /// Get source for a config path.
    pub fn get_source(&self, path: &str) -> ConfigSource {
        self.sources.get(path).copied().unwrap_or(ConfigSource::Default)
    }

    /// Get the effective model for the current provider.
    pub fn effective_model(&self) -> String {
        if !self.model.value.is_empty() {
            return self.model.value.clone();
        }

        if let Some(creds) = self.current_provider_credentials() {
            if let Some(ref model) = creds.model {
                if !model.is_empty() {
                    return model.clone();
                }
            }
        }

        if let Some(ref provider) = self.provider.value {
            if let Some(field) = provider.metadata().resolved_field("model") {
                if let Some(default) = field.default {
                    return default.to_string();
                }
            }
        }

        String::new()
    }

    /// Get the effective max_tokens for the current provider.
    pub fn effective_max_tokens(&self) -> Option<u32> {
        if self.max_tokens.value.is_some() {
            return self.max_tokens.value;
        }

        if let Some(creds) = self.current_provider_credentials() {
            if creds.max_tokens.is_some() {
                return creds.max_tokens;
            }
        }

        None
    }

    // ========================================================================
    // Validation
    // ========================================================================

    /// Validate configuration for the current provider.
    pub fn validate_provider(&self) -> Vec<ValidationError> {
        let provider = match &self.provider.value {
            Some(p) => p,
            None => return vec![],
        };

        let meta = provider.metadata();
        let creds = self.providers.get(provider).unwrap();
        let mut errors = Vec::new();

        for field in meta.all_fields() {
            if !field.required {
                continue;
            }

            let value = creds.get_field(field.name);
            let is_missing = value.map(|v| v.is_empty()).unwrap_or(true);

            if is_missing {
                let hint = if let Some(env_var) = field.env_var {
                    format!(
                        "Set {} or add [{}].{} to config.toml",
                        env_var, meta.name, field.name
                    )
                } else {
                    format!("Add [{}].{} to config.toml", meta.name, field.name)
                };

                errors.push(ValidationError {
                    field: field.name.to_string(),
                    description: field.description.to_string(),
                    hint,
                });
            }
        }

        errors
    }

    /// Validate configuration and return a `ValidatedConfig` on success.
    pub fn validate(&self) -> anyhow::Result<ValidatedConfig<'_>> {
        // Check for SHAI_SKIP_CONFIRM / SHAI_FRONTEND conflict
        if let Ok(skip_confirm) = std::env::var(env::SHAI_SKIP_CONFIRM) {
            if skip_confirm.to_lowercase() == "true" {
                if let Ok(frontend) = std::env::var(env::SHAI_FRONTEND) {
                    if !frontend.is_empty() && frontend.to_lowercase() != "noninteractive" {
                        anyhow::bail!(
                            "Configuration conflict: {}=true and {}={} are mutually exclusive.\n\
                             Either unset {} or set {}=noninteractive.",
                            env::SHAI_SKIP_CONFIRM, env::SHAI_FRONTEND, frontend,
                            env::SHAI_SKIP_CONFIRM, env::SHAI_FRONTEND
                        );
                    }
                }
            }
        }

        // Check if provider is set
        let provider = match &self.provider.value {
            Some(p) => p,
            None => {
                let provider_names: Vec<&str> = PROVIDER_METADATA.iter().map(|p| p.name).collect();
                anyhow::bail!(
                    "No provider configured.\n\n\
                     Quick start (choose one):\n  \
                     1. Set environment variable:  export {}=groq\n  \
                     2. Generate config file:      shell-ai config init\n  \
                     3. View all options:          shell-ai config schema\n\n\
                     Supported providers: {}",
                    env::SHAI_API_PROVIDER,
                    provider_names.join(", ")
                );
            }
        };

        let errors = self.validate_provider();
        if !errors.is_empty() {
            let meta = provider.metadata();
            let mut msg = format!(
                "Configuration incomplete for {} provider:",
                meta.display_name
            );

            for err in &errors {
                msg.push_str(&format!("\n  - {}: {}", err.field, err.description));
                msg.push_str(&format!("\n    Hint: {}", err.hint));
            }

            anyhow::bail!("{}", msg);
        }

        let credentials = self.providers.get(provider)
            .expect("credentials exist after validate_provider passes");

        Ok(ValidatedConfig {
            config: self,
            provider,
            credentials,
        })
    }

    // ========================================================================
    // Field Accessors for Data-Driven Config Display
    // ========================================================================

    fn get_global_field_display(&self, name: &str) -> Option<(String, ConfigSource)> {
        match name {
            "provider" => {
                let value = self.provider.value.as_ref()
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "(not set)".to_string());
                Some((value, self.provider.source))
            }
            "model" => {
                let effective_model = self.effective_model();
                // Track source: global model → provider-specific model → default
                let source = if !self.model.value.is_empty() {
                    self.model.source
                } else if let Some(provider) = self.provider.value.as_ref() {
                    let path = format!("{}.model", provider.metadata().name);
                    self.get_source(&path)
                } else {
                    ConfigSource::Default
                };
                let display = if effective_model.is_empty() {
                    "(not set)".to_string()
                } else {
                    effective_model
                };
                Some((display, source))
            }
            "temperature" => Some((format!("{:.2}", self.temperature.value), self.temperature.source)),
            "suggestion_count" => Some((self.suggestion_count.value.to_string(), self.suggestion_count.source)),
            "skip_confirm" => {
                if let Ok(v) = std::env::var(env::SHAI_SKIP_CONFIRM) {
                    if v.to_lowercase() == "true" {
                        return Some(("true".to_string(), ConfigSource::Environment));
                    }
                }
                Some(("false".to_string(), ConfigSource::Default))
            }
            "frontend" => Some((self.frontend.value.to_string(), self.frontend.source)),
            "output_format" => Some((self.output_format.value.to_string(), self.output_format.source)),
            "max_reference_chars" => Some((self.max_reference_chars.value.to_string(), self.max_reference_chars.source)),
            "max_tokens" => {
                let effective = self.effective_max_tokens();
                // Track source: global max_tokens → provider-specific max_tokens → default
                let source = if self.max_tokens.value.is_some() {
                    self.max_tokens.source
                } else if let Some(provider) = self.provider.value.as_ref() {
                    let path = format!("{}.max_tokens", provider.metadata().name);
                    self.get_source(&path)
                } else {
                    ConfigSource::Default
                };
                let display = effective
                    .map(|t| t.to_string())
                    .unwrap_or_else(|| "(not set)".to_string());
                Some((display, source))
            }
            "debug" => {
                let value = self.debug.value
                    .map(|d| d.to_string())
                    .unwrap_or_else(|| "(not set)".to_string());
                Some((value, self.debug.source))
            }
            _ => None,
        }
    }

    fn get_provider_field_display(&self, field: &FieldMeta, creds: &ProviderCredentials, provider_name: &str) -> (String, ConfigSource) {
        match creds.get_field(field.name) {
            Some(v) if !v.is_empty() => {
                let path = format!("{}.{}", provider_name, field.name);
                let source = self.get_source(&path);
                (v, source)
            }
            _ => ("(not set)".to_string(), ConfigSource::Default),
        }
    }

    fn get_providers_to_display(&self) -> Vec<Provider> {
        let mut result = Vec::new();

        // First, add the active provider if set
        if let Some(ref active) = self.provider.value {
            result.push(*active);
        }

        // Then add any other providers that have non-default credentials
        // Iterate in PROVIDER_METADATA order for stable output
        for meta in PROVIDER_METADATA {
            let provider = Provider::from_str(meta.name).unwrap();
            if self.provider.value.as_ref() == Some(&provider) {
                continue;
            }
            if let Some(creds) = self.providers.get(&provider) {
                if self.has_non_default_credentials(&provider, creds) {
                    result.push(provider);
                }
            }
        }

        result
    }

    fn has_non_default_credentials(&self, provider: &Provider, creds: &ProviderCredentials) -> bool {
        let meta = provider.metadata();
        for field in meta.all_fields() {
            let current_value = creds.get_field(field.name);
            let default_value = field.default;

            match (current_value.as_deref(), default_value) {
                (Some(val), None) if !val.is_empty() => return true,
                (Some(val), Some(def)) if val != def => return true,
                _ => {}
            }
        }
        false
    }

    /// Print configuration in human-readable format.
    pub fn print_human(&self) {
        println!("{}", "Shell-AI Configuration".bold());
        println!("{}", "======================".bold());
        println!();

        let sections = [
            Section::Provider,
            Section::Ui,
            Section::Suggest,
            Section::Explain,
        ];

        for section in sections {
            println!("{}:", section.title().cyan());
            for field in GLOBAL_SETTINGS_METADATA.iter().filter(|f| f.section == section) {
                if let Some((value, source)) = self.get_global_field_display(field.name) {
                    if field.deprecated && source == ConfigSource::Default {
                        continue;
                    }
                    let display_value = if field.sensitive {
                        mask_value(&value)
                    } else {
                        value
                    };
                    print_config_line_deprecated(field.name, &display_value, source, field.deprecated);
                }
            }
            println!();
        }

        // Provider-specific settings
        let providers_to_show = self.get_providers_to_display();
        for provider in providers_to_show {
            let meta = provider.metadata();
            println!("{}:", format!("{} Settings", meta.display_name).cyan());
            if let Some(creds) = self.providers.get(&provider) {
                for field in meta.all_fields() {
                    let (value, source) = self.get_provider_field_display(&field, creds, meta.name);
                    let display_value = if field.sensitive {
                        mask_value(&value)
                    } else {
                        value
                    };
                    print_config_line(field.name, &display_value, source);
                }
            }
            println!();
        }

        // Config files section
        println!("{}:", "Config Files".cyan());
        let toml_path = toml_config_path();
        let toml_status = match (&self.toml_path, &toml_path) {
            (Some(p), _) => format!("{} (loaded)", p.display()),
            (None, Some(p)) => format!("{} {}", p.display(), file_status(p).dimmed()),
            (None, None) => "(path unavailable)".to_string(),
        };
        println!("  {}: {}", "TOML".white(), toml_status);

        let json_path = json_config_path();
        let json_status = match (&self.json_path, &json_path) {
            (Some(p), _) => format!("{} (loaded, legacy)", p.display()),
            (None, Some(p)) => format!("{} {}", p.display(), file_status(p).dimmed()),
            (None, None) => "(path unavailable)".to_string(),
        };
        println!("  {}: {}", "JSON".white(), json_status);
    }

    /// Print configuration in JSON format.
    pub fn print_json(&self) {
        let mut global_settings = serde_json::Map::new();
        for field in GLOBAL_SETTINGS_METADATA {
            if let Some((value, source)) = self.get_global_field_display(field.name) {
                if field.deprecated && source == ConfigSource::Default {
                    continue;
                }
                let display_value = if field.sensitive {
                    mask_value(&value)
                } else {
                    value
                };
                global_settings.insert(field.name.to_string(), serde_json::json!({
                    "value": display_value,
                    "source": source.to_string(),
                    "deprecated": field.deprecated,
                }));
            }
        }

        let mut provider_settings = serde_json::Map::new();
        for provider in self.get_providers_to_display() {
            let meta = provider.metadata();
            if let Some(creds) = self.providers.get(&provider) {
                let mut fields = serde_json::Map::new();
                for field in meta.all_fields() {
                    let (value, source) = self.get_provider_field_display(&field, creds, meta.name);
                    let display_value = if field.sensitive {
                        mask_value(&value)
                    } else {
                        value
                    };
                    fields.insert(field.name.to_string(), serde_json::json!({
                        "value": display_value,
                        "source": source.to_string(),
                    }));
                }
                provider_settings.insert(meta.name.to_string(), serde_json::Value::Object(fields));
            }
        }

        let json = serde_json::json!({
            "global": global_settings,
            "providers": provider_settings,
            "config_files": {
                "toml": {
                    "path": toml_config_path().map(|p| p.display().to_string()),
                    "exists": self.toml_path.is_some(),
                },
                "json": {
                    "path": json_config_path().map(|p| p.display().to_string()),
                    "exists": self.json_path.is_some(),
                },
            },
        });
        println!("{}", serde_json::to_string_pretty(&json).unwrap());
    }

    // ========================================================================
    // Config Init and Schema
    // ========================================================================

    pub fn generate_init_config() -> String {
        use std::fmt::Write;
        let mut output = String::new();

        writeln!(output, "# Shell-AI Configuration").unwrap();
        writeln!(output, "# Generated by: shell-ai config init").unwrap();
        writeln!(output, "#").unwrap();
        writeln!(output, "# Configuration precedence (highest to lowest):").unwrap();
        writeln!(output, "#   1. CLI flags (--provider, --model, etc.)").unwrap();
        writeln!(output, "#   2. Environment variables").unwrap();
        writeln!(output, "#   3. This config file").unwrap();
        writeln!(output, "#   4. Built-in defaults").unwrap();
        writeln!(output).unwrap();

        // Global settings
        writeln!(output, "# ===========================================================================").unwrap();
        writeln!(output, "# Global Settings").unwrap();
        writeln!(output, "# ===========================================================================").unwrap();
        writeln!(output).unwrap();

        for field in GLOBAL_SETTINGS_METADATA {
            if field.virtual_field {
                continue;
            }
            write_field_description(&mut output, field);
            write_field_default(&mut output, field, None);
            writeln!(output).unwrap();
        }

        // Provider sections
        writeln!(output, "# ===========================================================================").unwrap();
        writeln!(output, "# Provider Configurations").unwrap();
        writeln!(output, "# ===========================================================================").unwrap();
        writeln!(output, "# Uncomment and configure the provider(s) you want to use.").unwrap();
        writeln!(output).unwrap();

        for provider in PROVIDER_METADATA {
            writeln!(output, "# ---------------------------------------------------------------------------").unwrap();
            writeln!(output, "# {} - {}", provider.display_name, provider.description).unwrap();
            writeln!(output, "# ---------------------------------------------------------------------------").unwrap();
            writeln!(output, "[{}]", provider.name).unwrap();

            for field in provider.all_fields() {
                write_field_description(&mut output, &field);
                if field.required {
                    writeln!(output, "# REQUIRED").unwrap();
                }
                let placeholder = if field.sensitive { Some("your-api-key-here") } else { None };
                write_field_default(&mut output, &field, placeholder);
                writeln!(output).unwrap();
            }
            writeln!(output).unwrap();
        }

        output
    }

    pub fn write_init_config(to_stdout: bool) -> anyhow::Result<()> {
        use std::io::Write;

        let content = Self::generate_init_config();

        if to_stdout {
            print!("{}", content);
            return Ok(());
        }

        let path = toml_config_path()
            .ok_or_else(|| anyhow::anyhow!("Could not determine config directory"))?;

        if path.exists() {
            anyhow::bail!(
                "Config file already exists at: {}\nUse --stdout to print to stdout instead.",
                path.display()
            );
        }

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut file = fs::File::create(&path)?;
        file.write_all(content.as_bytes())?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = file.metadata()?.permissions();
            perms.set_mode(0o600);
            fs::set_permissions(&path, perms)?;
        }

        println!("Created config file at: {}", path.display());
        println!("Edit this file to configure your providers.");

        Ok(())
    }

    pub fn print_schema(output_format: OutputFormat) {
        let provider_values: Vec<&str> = PROVIDER_METADATA.iter().map(|p| p.name).collect();
        let frontend_values: Vec<String> = Frontend::iter().map(|f| f.to_string()).collect();
        let output_format_values: Vec<String> = OutputFormat::iter().map(|o| o.to_string()).collect();

        match output_format {
            OutputFormat::Human => {
                println!("{}", "Shell-AI Configuration Schema".bold());
                println!("{}", "=".repeat(60));
                println!();

                println!("{}", "Global Settings".cyan().bold());
                println!("{}", "-".repeat(40));
                for field in GLOBAL_SETTINGS_METADATA {
                    if field.virtual_field {
                        continue;
                    }
                    println!("  {}", field.name.white().bold());
                    println!("    {}", field.description);
                    if let Some(env) = field.env_var {
                        println!("    Env: {}", env.green());
                    }
                    if let Some(default) = field.default {
                        println!("    Default: {}", default.dimmed());
                    }
                    println!();
                }

                println!("{}", "Valid Values".cyan().bold());
                println!("{}", "-".repeat(40));
                println!("  {}: {}", "provider".white().bold(), provider_values.join(", "));
                println!("  {}: {}", "frontend".white().bold(), frontend_values.join(", "));
                println!("  {}: {}", "output_format".white().bold(), output_format_values.join(", "));
                println!();

                println!("{}", "Provider Settings".cyan().bold());
                println!("{}", "-".repeat(40));

                for provider in PROVIDER_METADATA {
                    println!();
                    println!("  {} [{}]", provider.display_name.white().bold(), provider.name);
                    println!("    {}", provider.description.dimmed());
                    println!();

                    for field in provider.all_fields() {
                        let req_marker = if field.required {
                            " (required)".red().to_string()
                        } else {
                            String::new()
                        };
                        println!("    {}{}", field.name.white(), req_marker);
                        println!("      {}", field.description);
                        if let Some(env) = field.env_var {
                            println!("      Env: {}", env.green());
                        }
                        if let Some(default) = field.default {
                            println!("      Default: {}", default.dimmed());
                        }
                    }
                }
                println!();
            }
            OutputFormat::Json => {
                let schema = serde_json::json!({
                    "global_settings": GLOBAL_SETTINGS_METADATA.iter()
                        .filter(|f| !f.virtual_field)
                        .map(|f| {
                            serde_json::json!({
                                "name": f.name,
                                "description": f.description,
                                "env_var": f.env_var,
                                "default": f.default,
                                "required": f.required,
                            })
                        }).collect::<Vec<_>>(),
                    "valid_values": {
                        "provider": provider_values,
                        "frontend": frontend_values,
                        "output_format": output_format_values,
                    },
                    "providers": PROVIDER_METADATA.iter().map(|p| {
                        serde_json::json!({
                            "name": p.name,
                            "display_name": p.display_name,
                            "description": p.description,
                            "fields": p.all_fields().map(|f| {
                                serde_json::json!({
                                    "name": f.name,
                                    "description": f.description,
                                    "env_var": f.env_var,
                                    "default": f.default,
                                    "required": f.required,
                                })
                            }).collect::<Vec<_>>(),
                        })
                    }).collect::<Vec<_>>(),
                });

                println!("{}", serde_json::to_string_pretty(&schema).unwrap());
            }
        }
    }
}

fn print_config_line(name: &str, value: &str, source: ConfigSource) {
    let source_str = format!("[{}]", source);
    println!(
        "  {:20} {:20} {}",
        name.white(),
        value.green(),
        source_str.dimmed()
    );
}

fn print_config_line_deprecated(name: &str, value: &str, source: ConfigSource, deprecated: bool) {
    let source_str = format!("[{}]", source);
    let deprecated_marker = if deprecated { " (deprecated)".yellow().to_string() } else { String::new() };
    println!(
        "  {:20} {:20} {}{}",
        name.white(),
        value.green(),
        source_str.dimmed(),
        deprecated_marker
    );
}

fn file_status(path: &PathBuf) -> String {
    use std::io::ErrorKind;
    match fs::metadata(path) {
        Ok(_) => "(exists but unreadable)".to_string(),
        Err(e) => match e.kind() {
            ErrorKind::NotFound => "(not found)".to_string(),
            ErrorKind::PermissionDenied => "(permission denied)".to_string(),
            _ => format!("({})", e.kind()),
        },
    }
}

fn mask_value(value: &str) -> String {
    if value.is_empty() || value == "(not set)" {
        return value.to_string();
    }
    if value.len() > 6 {
        format!("****{}", &value[value.len() - 6..])
    } else {
        "****".to_string()
    }
}

fn write_field_description(output: &mut String, field: &FieldMeta) {
    use std::fmt::Write;
    if let Some(env_var) = field.env_var {
        writeln!(output, "# {} (env: {})", field.description, env_var).unwrap();
    } else {
        writeln!(output, "# {}", field.description).unwrap();
    }
}

fn write_field_default(output: &mut String, field: &FieldMeta, placeholder: Option<&str>) {
    use std::fmt::Write;
    let value = placeholder.or(field.default).unwrap_or("");
    if value.is_empty() {
        writeln!(output, "# {} = \"\"", field.name).unwrap();
    } else {
        writeln!(output, "# {} = \"{}\"", field.name, value).unwrap();
    }
}

pub fn toml_config_path() -> Option<PathBuf> {
    let mut base = dirs::config_dir()?;
    base.push("shell-ai");
    base.push("config.toml");
    Some(base)
}

pub fn json_config_path() -> Option<PathBuf> {
    let mut base = dirs::config_dir()?;
    base.push("shell-ai");
    base.push("config.json");
    Some(base)
}

enum TomlJsonLoadResult {
    Loaded(serde_json::Value, PathBuf),
    NotFound,
    ParseError(PathBuf, String),
}

fn load_toml_as_json() -> TomlJsonLoadResult {
    let path = match toml_config_path() {
        Some(p) => p,
        None => return TomlJsonLoadResult::NotFound,
    };

    let data = match fs::read_to_string(&path) {
        Ok(d) => d,
        Err(_) => return TomlJsonLoadResult::NotFound,
    };

    let toml_value: toml::Value = match toml::from_str(&data) {
        Ok(v) => v,
        Err(e) => return TomlJsonLoadResult::ParseError(path, e.to_string()),
    };

    let json_value = toml_to_json(&toml_value);
    TomlJsonLoadResult::Loaded(json_value, path)
}

fn toml_to_json(toml: &toml::Value) -> serde_json::Value {
    match toml {
        toml::Value::String(s) => serde_json::Value::String(s.clone()),
        toml::Value::Integer(i) => serde_json::json!(*i),
        toml::Value::Float(f) => serde_json::json!(*f),
        toml::Value::Boolean(b) => serde_json::Value::Bool(*b),
        toml::Value::Datetime(dt) => serde_json::Value::String(dt.to_string()),
        toml::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(toml_to_json).collect())
        }
        toml::Value::Table(table) => {
            let mut map = serde_json::Map::new();
            for (k, v) in table {
                map.insert(k.clone(), toml_to_json(v));
            }
            serde_json::Value::Object(map)
        }
    }
}

fn load_json_as_value() -> JsonValueLoadResult {
    let path = match json_config_path() {
        Some(p) => p,
        None => return JsonValueLoadResult::NotFound,
    };

    let data = match fs::read_to_string(&path) {
        Ok(d) => d,
        Err(_) => return JsonValueLoadResult::NotFound,
    };

    match serde_json::from_str(&data) {
        Ok(v) => JsonValueLoadResult::Loaded(v, path),
        Err(e) => JsonValueLoadResult::ParseError(path, e.to_string()),
    }
}

enum JsonValueLoadResult {
    Loaded(serde_json::Value, PathBuf),
    NotFound,
    ParseError(PathBuf, String),
}
