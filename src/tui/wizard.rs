//! First-run setup wizard. When no `~/.config/oli/config.toml`
//! exists at TUI startup, this overlay walks the user through:
//! pick provider → (paste api key) → confirm → save.
//!
//! The wizard's effect doesn't apply to the current session
//! (the agent is already constructed against env-var fallbacks
//! by the time we run); after Save the user gets a hint to
//! restart. That's the price of a config-driven harness — the
//! alternative would be a much bigger refactor to swap the
//! provider mid-session.

use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WizardStep {
    Welcome,
    PickProvider,
    EnterApiKey,
    Confirm,
    Saved { path: PathBuf },
}

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
}

#[derive(Clone, Debug)]
pub struct WizardState {
    pub step: WizardStep,
    pub provider_idx: usize,
    pub api_key: String,
}

impl Default for WizardState {
    fn default() -> Self {
        Self {
            step: WizardStep::Welcome,
            provider_idx: 0,
            api_key: String::new(),
        }
    }
}

impl WizardState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn current_provider(&self) -> WizardProvider {
        WizardProvider::all()[self.provider_idx.min(2)]
    }

    pub fn navigate_provider(&mut self, delta: i32) {
        let n = WizardProvider::all().len() as i32;
        let next = (self.provider_idx as i32 + delta).rem_euclid(n);
        self.provider_idx = next as usize;
    }

    /// Step forward in the wizard. The provider's
    /// `needs_api_key` flag governs whether step 2 (key entry)
    /// is included.
    pub fn advance(&mut self) {
        self.step = match self.step {
            WizardStep::Welcome => WizardStep::PickProvider,
            WizardStep::PickProvider => {
                if self.current_provider().needs_api_key() {
                    WizardStep::EnterApiKey
                } else {
                    WizardStep::Confirm
                }
            }
            WizardStep::EnterApiKey => WizardStep::Confirm,
            WizardStep::Confirm => self.step.clone(),
            WizardStep::Saved { .. } => self.step.clone(),
        };
    }

    /// Step back. Useful for "let me change my mind."
    pub fn step_back(&mut self) {
        self.step = match self.step {
            WizardStep::Welcome => WizardStep::Welcome,
            WizardStep::PickProvider => WizardStep::Welcome,
            WizardStep::EnterApiKey => WizardStep::PickProvider,
            WizardStep::Confirm => {
                if self.current_provider().needs_api_key() {
                    WizardStep::EnterApiKey
                } else {
                    WizardStep::PickProvider
                }
            }
            WizardStep::Saved { .. } => self.step.clone(),
        };
    }

    /// Render a TOML config string for the current selections.
    /// The format mirrors `specs/README.md`'s minimal example so
    /// users can compare against the spec and edit by hand later.
    pub fn render_toml(&self) -> String {
        let p = self.current_provider();
        let mut out = String::new();
        out.push_str(&format!("default_provider = \"{}\"\n\n", p.name()));
        out.push_str(&format!("[providers.{}]\n", p.name()));
        out.push_str(&format!("kind          = \"{}\"\n", p.config_kind()));
        out.push_str(&format!("base_url      = \"{}\"\n", p.base_url()));
        let key = if p.needs_api_key() {
            self.api_key.as_str()
        } else {
            // Ollama ignores the value but our schema currently
            // requires *something*. Use a recognizable
            // placeholder.
            "ollama"
        };
        out.push_str(&format!("api_key       = \"{}\"\n", key));
        out.push_str(&format!("default_model = \"{}\"\n", p.default_model()));
        out
    }
}

/// Where the config gets saved. Same path the rest of the
/// harness reads from.
pub fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("oli").join("config.toml"))
}

/// Write the rendered TOML to `path` (creating parent dirs).
/// Returns the actual path on success so the caller can show it
/// in the SystemNote. If the file already exists we *don't*
/// clobber — refuse and surface a hint. Wizards triggered on
/// "no config detected" startup won't hit this; the safety net
/// matters if the user keeps re-opening the wizard.
pub fn save(path: &std::path::Path, body: &str) -> Result<(), String> {
    if path.exists() {
        return Err(format!(
            "{} already exists; not overwriting. Edit it by hand instead.",
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

    #[test]
    fn welcome_advances_to_pick_provider() {
        let mut w = WizardState::new();
        assert!(matches!(w.step, WizardStep::Welcome));
        w.advance();
        assert!(matches!(w.step, WizardStep::PickProvider));
    }

    #[test]
    fn ollama_skips_api_key_step() {
        let mut w = WizardState::new();
        w.advance(); // PickProvider
        // provider_idx=0 → Ollama by default.
        w.advance();
        assert!(
            matches!(w.step, WizardStep::Confirm),
            "ollama should skip key entry"
        );
    }

    #[test]
    fn openrouter_includes_api_key_step() {
        let mut w = WizardState::new();
        w.advance(); // PickProvider
        w.navigate_provider(1); // OpenRouter
        w.advance(); // EnterApiKey
        assert!(matches!(w.step, WizardStep::EnterApiKey));
        w.advance(); // Confirm
        assert!(matches!(w.step, WizardStep::Confirm));
    }

    #[test]
    fn step_back_undoes_advance() {
        let mut w = WizardState::new();
        w.advance(); // PickProvider
        w.navigate_provider(1); // OpenRouter
        w.advance(); // EnterApiKey
        w.step_back();
        assert!(matches!(w.step, WizardStep::PickProvider));
        w.step_back();
        assert!(matches!(w.step, WizardStep::Welcome));
    }

    #[test]
    fn provider_navigation_wraps() {
        let mut w = WizardState::new();
        w.navigate_provider(-1);
        assert_eq!(w.current_provider(), WizardProvider::Anthropic);
        w.navigate_provider(1);
        assert_eq!(w.current_provider(), WizardProvider::Ollama);
    }

    #[test]
    fn render_toml_for_ollama_uses_placeholder_key() {
        let w = WizardState::new();
        let body = w.render_toml();
        assert!(body.contains("default_provider = \"ollama\""));
        assert!(body.contains("[providers.ollama]"));
        assert!(body.contains("api_key       = \"ollama\""));
        assert!(body.contains("default_model = \"qwen"));
    }

    #[test]
    fn render_toml_for_openrouter_includes_api_key() {
        let mut w = WizardState::new();
        w.navigate_provider(1);
        w.api_key = "sk-or-test-key".into();
        let body = w.render_toml();
        assert!(body.contains("default_provider = \"openrouter\""));
        assert!(body.contains("kind          = \"openai-compat\""));
        assert!(body.contains("api_key       = \"sk-or-test-key\""));
    }

    #[test]
    fn render_toml_for_anthropic_uses_anthropic_kind() {
        let mut w = WizardState::new();
        w.navigate_provider(2);
        w.api_key = "sk-ant-x".into();
        let body = w.render_toml();
        assert!(body.contains("kind          = \"anthropic\""));
        assert!(body.contains("base_url      = \"https://api.anthropic.com\""));
    }

    #[test]
    fn save_refuses_to_overwrite_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "preexisting").unwrap();
        let err = save(&path, "new").unwrap_err();
        assert!(err.contains("already exists"));
        // Original content untouched.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "preexisting");
    }

    #[test]
    fn save_writes_through_with_parent_dir_creation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("config.toml");
        save(&path, "body\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "body\n");
    }
}
