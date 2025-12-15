use crate::config::{Provider, ValidatedConfig};

/// Provider configuration for making API requests.
#[derive(Clone)]
pub struct ProviderConfig {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub temperature: f32,
    /// Extra headers (e.g., Azure's api-key, OpenAI's OpenAI-Organization).
    pub extra_headers: Vec<(String, String)>,
    /// Max tokens for AI response (optional, API auto-calculates when None).
    pub max_tokens: Option<u32>,
}

impl ProviderConfig {
    /// Build provider config from a validated configuration.
    ///
    /// This takes a `ValidatedConfig` which guarantees at compile time that
    /// the provider and credentials exist. No `Result` needed - the types
    /// enforce that validation has occurred.
    pub fn from_validated(validated: &ValidatedConfig) -> Self {
        let temperature = validated.temperature();
        let max_tokens = validated.effective_max_tokens();
        let provider = validated.provider;
        let creds = validated.credentials;

        match provider {
            Provider::OpenAI => {
                let base = creds.api_base.clone()
                    .unwrap_or_else(|| "https://api.openai.com".to_string());
                let mut extra_headers = Vec::new();
                if let Some(ref org) = creds.organization {
                    extra_headers.push(("OpenAI-Organization".to_string(), org.clone()));
                }
                ProviderConfig {
                    base_url: base,
                    model: validated.effective_model(),
                    api_key: creds.api_key.clone(),
                    temperature,
                    extra_headers,
                    max_tokens,
                }
            }
            Provider::Azure => {
                // Azure has special handling for URL format and can fall back to openai.api_key
                let base = creds.api_base.clone().unwrap_or_default();
                let deployment = creds.deployment_name.clone().unwrap_or_default();
                let api_version = creds.api_version.clone()
                    .unwrap_or_else(|| "2023-05-15".to_string());
                let api_key = creds.api_key.clone()
                    .or_else(|| {
                        validated.app_config()
                            .get_credentials_for(&Provider::OpenAI)
                            .and_then(|c| c.api_key.clone())
                    });

                let url = format!(
                    "{}/openai/deployments/{}/chat/completions?api-version={}",
                    base.trim_end_matches('/'), deployment, api_version
                );

                let header_val = api_key.clone().unwrap_or_default();

                ProviderConfig {
                    base_url: url,
                    model: String::new(), // Azure uses deployment name, not model
                    api_key,
                    temperature,
                    extra_headers: vec![("api-key".to_string(), header_val)],
                    max_tokens,
                }
            }
            Provider::Ollama => {
                let base = creds.api_base.clone()
                    .unwrap_or_else(|| "http://localhost:11434".to_string());
                ProviderConfig {
                    base_url: base,
                    model: validated.effective_model(),
                    api_key: Some("ollama".to_string()), // Ollama requires a dummy key
                    temperature,
                    extra_headers: vec![],
                    max_tokens,
                }
            }
            Provider::Mistral => {
                let base = creds.api_base.clone()
                    .unwrap_or_else(|| "https://api.mistral.ai".to_string());
                ProviderConfig {
                    base_url: base,
                    model: validated.effective_model(),
                    api_key: creds.api_key.clone(),
                    temperature,
                    extra_headers: vec![],
                    max_tokens,
                }
            }
            Provider::Groq => {
                let base = creds.api_base.clone()
                    .unwrap_or_else(|| "https://api.groq.com/openai".to_string());
                ProviderConfig {
                    base_url: base,
                    model: validated.effective_model(),
                    api_key: creds.api_key.clone(),
                    temperature,
                    extra_headers: vec![],
                    max_tokens,
                }
            }
        }
    }

    /// Get the chat completions URL for this provider.
    pub fn chat_completions_url(&self) -> String {
        if self.base_url.contains("/chat/completions") {
            self.base_url.clone()
        } else {
            format!("{}/v1/chat/completions", self.base_url.trim_end_matches('/'))
        }
    }

    /// Get extra headers as borrowed string slices for use with http functions.
    pub fn extra_headers_ref(&self) -> Vec<(&str, &str)> {
        self.extra_headers.iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect()
    }
}