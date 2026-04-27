use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::{AgentError, Result};

/// Top-level harness config. Phase 0 only models the providers section;
/// later phases will add `[policy]`, `[plugins]`, `[[tools.subprocess]]`, etc.
#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    pub default_provider: String,

    #[serde(default)]
    pub default_model: Option<String>,

    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProviderConfig {
    pub kind: String,
    pub base_url: String,

    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub default_model: Option<String>,
}

impl Config {
    pub fn from_str(s: &str) -> Result<Self> {
        toml::from_str(s).map_err(|e| AgentError::Config(e.to_string()))
    }

    pub fn from_file(path: &Path) -> Result<Self> {
        let body = std::fs::read_to_string(path)?;
        Self::from_str(&body)
    }

    /// Load `~/.config/agent/config.toml` if present, else fall back to an
    /// env-only default that preserves byte-identical behavior with the
    /// pre-Phase-0 binary (OpenRouter via `OPENROUTER_API_KEY`, model
    /// `anthropic/claude-haiku-4.5`).
    pub fn load_or_default() -> Result<Self> {
        if let Some(p) = default_config_path()
            && p.exists()
        {
            return Self::from_file(&p);
        }
        Ok(Self::env_default())
    }

    pub fn env_default() -> Self {
        let base_url = std::env::var("OPENROUTER_BASE_URL")
            .unwrap_or_else(|_| "https://openrouter.ai/api/v1".to_string());
        let mut providers = HashMap::new();
        providers.insert(
            "openrouter".to_string(),
            ProviderConfig {
                kind: "openai-compat".to_string(),
                base_url,
                api_key: None,
                api_key_env: Some("OPENROUTER_API_KEY".to_string()),
                default_model: Some("anthropic/claude-haiku-4.5".to_string()),
            },
        );
        Self {
            default_provider: "openrouter".to_string(),
            default_model: None,
            providers,
        }
    }

    pub fn provider(&self, name: &str) -> Result<&ProviderConfig> {
        self.providers
            .get(name)
            .ok_or_else(|| AgentError::Config(format!("unknown provider: {}", name)))
    }

    /// Resolve the API key for a provider. `api_key_env` wins over `api_key`
    /// so users can override a literal key by exporting an env var.
    pub fn resolve_api_key(&self, provider_name: &str) -> Result<String> {
        let p = self.provider(provider_name)?;
        if let Some(env_key) = &p.api_key_env {
            return std::env::var(env_key).map_err(|_| {
                AgentError::Config(format!(
                    "env var {} is not set (required by provider '{}')",
                    env_key, provider_name
                ))
            });
        }
        if let Some(key) = &p.api_key {
            return Ok(key.clone());
        }
        Err(AgentError::Config(format!(
            "provider '{}' has neither api_key nor api_key_env",
            provider_name
        )))
    }

    /// Top-level `default_model` wins over provider-scoped `default_model`.
    pub fn model_for(&self, provider_name: &str) -> Result<String> {
        if let Some(m) = &self.default_model {
            return Ok(m.clone());
        }
        let p = self.provider(provider_name)?;
        p.default_model.clone().ok_or_else(|| {
            AgentError::Config(format!(
                "no default model configured for provider '{}'",
                provider_name
            ))
        })
    }
}

fn default_config_path() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(dir.join("agent").join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
        default_provider = "ollama"
        default_model    = "qwen2.5-coder:7b"

        [providers.ollama]
        kind     = "openai-compat"
        base_url = "http://localhost:11434/v1"
        api_key  = "ollama"

        [providers.openrouter]
        kind          = "openai-compat"
        base_url      = "https://openrouter.ai/api/v1"
        api_key_env   = "OPENROUTER_API_KEY"
        default_model = "anthropic/claude-haiku-4.5"
    "#;

    #[test]
    fn parses_sample_config() {
        let cfg = Config::from_str(SAMPLE).unwrap();
        assert_eq!(cfg.default_provider, "ollama");
        assert_eq!(cfg.default_model.as_deref(), Some("qwen2.5-coder:7b"));
        assert_eq!(cfg.providers.len(), 2);
        assert_eq!(cfg.providers["ollama"].kind, "openai-compat");
    }

    #[test]
    fn env_default_models_today_behavior() {
        let cfg = Config::env_default();
        assert_eq!(cfg.default_provider, "openrouter");
        let p = cfg.provider("openrouter").unwrap();
        assert_eq!(p.api_key_env.as_deref(), Some("OPENROUTER_API_KEY"));
        assert_eq!(
            p.default_model.as_deref(),
            Some("anthropic/claude-haiku-4.5")
        );
    }

    #[test]
    fn resolve_api_key_prefers_env_var() {
        // SAFETY: setting an env var with a unique name; threaded test isolation
        // is acceptable for Phase 0.
        unsafe {
            std::env::set_var("__AGENT_TEST_API_KEY", "from-env");
        }
        let mut cfg = Config::env_default();
        cfg.providers.get_mut("openrouter").unwrap().api_key_env =
            Some("__AGENT_TEST_API_KEY".into());
        assert_eq!(cfg.resolve_api_key("openrouter").unwrap(), "from-env");
        unsafe {
            std::env::remove_var("__AGENT_TEST_API_KEY");
        }
    }

    #[test]
    fn resolve_api_key_falls_back_to_literal() {
        let cfg = Config::from_str(SAMPLE).unwrap();
        // ollama provider has only `api_key`, no `api_key_env`.
        assert_eq!(cfg.resolve_api_key("ollama").unwrap(), "ollama");
    }

    #[test]
    fn resolve_api_key_errors_when_neither_set() {
        let toml = r#"
            default_provider = "x"
            [providers.x]
            kind     = "openai-compat"
            base_url = "http://example"
        "#;
        let cfg = Config::from_str(toml).unwrap();
        let err = cfg.resolve_api_key("x").unwrap_err();
        assert!(err.to_string().contains("neither api_key nor api_key_env"));
    }

    #[test]
    fn model_for_prefers_top_level_default() {
        let cfg = Config::from_str(SAMPLE).unwrap();
        assert_eq!(cfg.model_for("openrouter").unwrap(), "qwen2.5-coder:7b");
    }

    #[test]
    fn model_for_falls_back_to_provider_default() {
        let cfg = Config::env_default();
        assert_eq!(
            cfg.model_for("openrouter").unwrap(),
            "anthropic/claude-haiku-4.5"
        );
    }

    #[test]
    fn unknown_provider_is_config_error() {
        let cfg = Config::env_default();
        let err = cfg.provider("nope").unwrap_err();
        assert!(err.to_string().contains("unknown provider: nope"));
    }
}
