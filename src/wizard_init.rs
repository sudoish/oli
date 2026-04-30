//! First-run config bootstrap — shared between the TUI's
//! interactive wizard and the headless `oli init` CLI subcommand.
//!
//! The TUI side adds a step machine + key handler on top
//! ([`crate::tui::wizard`]); this module is the data layer:
//! provider definitions, TOML rendering, and the file-system
//! save path. Both surfaces produce the same config, so a user
//! who runs `oli init --provider ollama` and a user who picks
//! Ollama in the TUI end up with byte-identical files.

use std::path::{Path, PathBuf};

const FILE_NAME: &str = "config.toml";

/// Bundled provider templates. Each one knows its display
/// label, config `kind` (which provider impl to instantiate),
/// default model id, default base URL, and whether it needs an
/// API key. Adding a new template means adding an enum variant
/// and the matching arms below — no other call sites change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WizardProvider {
    Ollama,
    OpenRouter,
    Anthropic,
}

impl WizardProvider {
    pub fn label(self) -> &'static str {
        match self {
            WizardProvider::Ollama => "Ollama (local — no API key required)",
            WizardProvider::OpenRouter => "OpenRouter (Claude / GPT / etc — paid)",
            WizardProvider::Anthropic => "Anthropic (native Claude API — paid)",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            WizardProvider::Ollama => "ollama",
            WizardProvider::OpenRouter => "openrouter",
            WizardProvider::Anthropic => "anthropic",
        }
    }

    pub fn needs_api_key(self) -> bool {
        !matches!(self, WizardProvider::Ollama)
    }

    pub fn default_model(self) -> &'static str {
        match self {
            WizardProvider::Ollama => "qwen2.5-coder:7b",
            WizardProvider::OpenRouter => "anthropic/claude-haiku-4.5",
            WizardProvider::Anthropic => "claude-haiku-4-5",
        }
    }

    pub fn base_url(self) -> &'static str {
        match self {
            WizardProvider::Ollama => "http://localhost:11434/v1",
            WizardProvider::OpenRouter => "https://openrouter.ai/api/v1",
            WizardProvider::Anthropic => "https://api.anthropic.com",
        }
    }

    pub fn config_kind(self) -> &'static str {
        match self {
            WizardProvider::Ollama | WizardProvider::OpenRouter => "openai-compat",
            WizardProvider::Anthropic => "anthropic",
        }
    }

    pub fn all() -> [WizardProvider; 3] {
        [
            WizardProvider::Ollama,
            WizardProvider::OpenRouter,
            WizardProvider::Anthropic,
        ]
    }

    /// Parse a CLI / config string ("ollama" / "openrouter" /
    /// "anthropic"). Case-insensitive. Returns `None` for
    /// anything else so the caller can surface a usage error.
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "ollama" => Some(Self::Ollama),
            "openrouter" => Some(Self::OpenRouter),
            "anthropic" => Some(Self::Anthropic),
            _ => None,
        }
    }
}

/// Render a complete `config.toml` for the chosen provider +
/// optional API key. Mirrors the minimal example in
/// `specs/README.md` so a user can compare against the spec
/// and edit by hand later.
///
/// For Ollama the api_key field gets a recognizable
/// `"ollama"` placeholder — the schema requires *something*
/// non-empty even though the provider ignores the value.
pub fn render_toml(provider: WizardProvider, api_key: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!("default_provider = \"{}\"\n\n", provider.name()));
    out.push_str(&format!("[providers.{}]\n", provider.name()));
    out.push_str(&format!("kind          = \"{}\"\n", provider.config_kind()));
    out.push_str(&format!("base_url      = \"{}\"\n", provider.base_url()));
    let key = if provider.needs_api_key() {
        api_key
    } else {
        "ollama"
    };
    out.push_str(&format!("api_key       = \"{}\"\n", key));
    out.push_str(&format!(
        "default_model = \"{}\"\n",
        provider.default_model()
    ));
    out
}

/// Where the config goes. Same path the rest of the harness
/// reads from.
pub fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("oli").join(FILE_NAME))
}

/// Write the rendered TOML to `path`, creating parent dirs.
/// Refuses to clobber an existing file unless `force` is set —
/// a stray `oli init` should never silently overwrite a config
/// the user has been editing by hand.
pub fn save(path: &Path, body: &str, force: bool) -> Result<(), String> {
    if path.exists() && !force {
        return Err(format!(
            "{} already exists; pass --force to overwrite",
            path.display()
        ));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("create {}: {}", parent.display(), e))?;
    }
    std::fs::write(path, body).map_err(|e| format!("write {}: {}", path.display(), e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn from_name_is_case_insensitive() {
        assert_eq!(
            WizardProvider::from_name("Ollama"),
            Some(WizardProvider::Ollama)
        );
        assert_eq!(
            WizardProvider::from_name("OPENROUTER"),
            Some(WizardProvider::OpenRouter)
        );
        assert_eq!(WizardProvider::from_name("nope"), None);
    }

    #[test]
    fn render_toml_for_ollama_uses_placeholder_key_when_input_is_empty() {
        let body = render_toml(WizardProvider::Ollama, "");
        assert!(body.contains("default_provider = \"ollama\""));
        assert!(body.contains("api_key       = \"ollama\""));
        assert!(body.contains("default_model = \"qwen"));
    }

    #[test]
    fn render_toml_for_openrouter_uses_provided_key() {
        let body = render_toml(WizardProvider::OpenRouter, "sk-or-test");
        assert!(body.contains("kind          = \"openai-compat\""));
        assert!(body.contains("api_key       = \"sk-or-test\""));
    }

    #[test]
    fn render_toml_for_anthropic_uses_anthropic_kind() {
        let body = render_toml(WizardProvider::Anthropic, "sk-ant-x");
        assert!(body.contains("kind          = \"anthropic\""));
        assert!(body.contains("base_url      = \"https://api.anthropic.com\""));
    }

    #[test]
    fn save_refuses_to_overwrite_without_force() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "preexisting").unwrap();
        let err = save(&path, "new", false).unwrap_err();
        assert!(err.contains("already exists"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "preexisting");
    }

    #[test]
    fn save_overwrites_when_force_is_set() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "preexisting").unwrap();
        save(&path, "new body\n", true).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new body\n");
    }

    #[test]
    fn save_creates_parent_dir() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested").join("config.toml");
        save(&path, "body\n", false).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "body\n");
    }
}
