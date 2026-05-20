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
//!
//! Provider templates, TOML rendering, and the file-system
//! save path live in `crate::wizard_init` so the headless
//! `oli init` subcommand produces byte-identical output.

use std::path::PathBuf;

pub use crate::wizard_init::{WizardProvider, config_path};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WizardStep {
    Welcome,
    PickProvider,
    /// Ollama-only: shows daemon probe result + retry option.
    CheckDaemon,
    /// Ollama-only: shows whether the default model is pulled,
    /// streams pull progress when the user opts in.
    PullModel,
    EnterApiKey,
    Confirm,
    Saved { path: PathBuf },
}

#[derive(Clone, Debug, PartialEq)]
pub enum DaemonStatus {
    Unchecked,
    Probing,
    Down(String),
    Up { models: Vec<String> },
}

#[derive(Clone, Debug, PartialEq)]
pub enum PullStatus {
    /// User hasn't kicked off a pull and the model isn't known
    /// to be present (daemon may be down, or model just isn't
    /// installed).
    Idle,
    /// Pull is in flight. `phase` is the current Ollama status
    /// string ("downloading", "verifying sha256 digest", ...).
    InProgress {
        phase: String,
        completed: u64,
        total: u64,
    },
    Done,
    Failed(String),
    /// Daemon probe found the model already installed — nothing
    /// to do, the step is essentially a checkpoint.
    AlreadyPresent,
}

#[derive(Clone, Debug)]
pub struct WizardState {
    pub step: WizardStep,
    pub provider_idx: usize,
    pub api_key: String,
    pub daemon: DaemonStatus,
    pub pull: PullStatus,
}

impl Default for WizardState {
    fn default() -> Self {
        Self {
            step: WizardStep::Welcome,
            provider_idx: 0,
            api_key: String::new(),
            daemon: DaemonStatus::Unchecked,
            pull: PullStatus::Idle,
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
        // Switching provider invalidates any cached Ollama probe.
        self.daemon = DaemonStatus::Unchecked;
        self.pull = PullStatus::Idle;
    }

    /// Sync the `pull` field with the latest daemon probe — if
    /// the daemon is up and the chosen model is already there,
    /// the PullModel step shows `AlreadyPresent` instead of `Idle`.
    pub fn reconcile_pull_status(&mut self) {
        let model = self.current_provider().default_model();
        if let DaemonStatus::Up { models } = &self.daemon {
            if models.iter().any(|m| m == model) {
                self.pull = PullStatus::AlreadyPresent;
                return;
            }
        }
        if matches!(self.pull, PullStatus::AlreadyPresent) {
            self.pull = PullStatus::Idle;
        }
    }

    pub fn advance(&mut self) {
        self.step = match self.step {
            WizardStep::Welcome => WizardStep::PickProvider,
            WizardStep::PickProvider => {
                if matches!(self.current_provider(), WizardProvider::Ollama) {
                    WizardStep::CheckDaemon
                } else if self.current_provider().needs_api_key() {
                    WizardStep::EnterApiKey
                } else {
                    WizardStep::Confirm
                }
            }
            WizardStep::CheckDaemon => WizardStep::PullModel,
            WizardStep::PullModel => WizardStep::Confirm,
            WizardStep::EnterApiKey => WizardStep::Confirm,
            WizardStep::Confirm => self.step.clone(),
            WizardStep::Saved { .. } => self.step.clone(),
        };
    }

    pub fn step_back(&mut self) {
        self.step = match self.step {
            WizardStep::Welcome => WizardStep::Welcome,
            WizardStep::PickProvider => WizardStep::Welcome,
            WizardStep::CheckDaemon => WizardStep::PickProvider,
            WizardStep::PullModel => WizardStep::CheckDaemon,
            WizardStep::EnterApiKey => WizardStep::PickProvider,
            WizardStep::Confirm => {
                if matches!(self.current_provider(), WizardProvider::Ollama) {
                    WizardStep::PullModel
                } else if self.current_provider().needs_api_key() {
                    WizardStep::EnterApiKey
                } else {
                    WizardStep::PickProvider
                }
            }
            WizardStep::Saved { .. } => self.step.clone(),
        };
    }

    /// Render a TOML config string for the current selections.
    /// Delegates to `wizard_init::render_toml` so the TUI and
    /// the headless `oli init` produce byte-identical output.
    pub fn render_toml(&self) -> String {
        crate::wizard_init::render_toml(self.current_provider(), &self.api_key)
    }
}

/// Wizard-flavored save: refuses to clobber an existing file
/// (the wizard never gets here on the "no config detected"
/// startup path; the safety net matters if the user keeps
/// re-opening the wizard).
pub fn save(path: &std::path::Path, body: &str) -> Result<(), String> {
    crate::wizard_init::save(path, body, false)
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
    fn ollama_routes_through_check_daemon_and_pull_model() {
        let mut w = WizardState::new();
        w.advance(); // PickProvider
        w.advance(); // CheckDaemon
        assert!(matches!(w.step, WizardStep::CheckDaemon));
        w.advance(); // PullModel
        assert!(matches!(w.step, WizardStep::PullModel));
        w.advance(); // Confirm
        assert!(matches!(w.step, WizardStep::Confirm));
    }

    #[test]
    fn step_back_from_pull_model_returns_to_check_daemon() {
        let mut w = WizardState::new();
        w.advance(); // PickProvider
        w.advance(); // CheckDaemon
        w.advance(); // PullModel
        w.step_back();
        assert!(matches!(w.step, WizardStep::CheckDaemon));
        w.step_back();
        assert!(matches!(w.step, WizardStep::PickProvider));
    }

    #[test]
    fn step_back_from_confirm_on_ollama_returns_to_pull_model() {
        let mut w = WizardState::new();
        w.advance(); // PickProvider
        w.advance(); // CheckDaemon
        w.advance(); // PullModel
        w.advance(); // Confirm
        w.step_back();
        assert!(matches!(w.step, WizardStep::PullModel));
    }

    #[test]
    fn reconcile_pull_status_marks_already_present_when_daemon_has_model() {
        let mut w = WizardState::new();
        w.daemon = DaemonStatus::Up {
            models: vec!["qwen2.5-coder:7b".into()],
        };
        w.reconcile_pull_status();
        assert_eq!(w.pull, PullStatus::AlreadyPresent);
    }

    #[test]
    fn reconcile_pull_status_keeps_idle_when_model_missing() {
        let mut w = WizardState::new();
        w.daemon = DaemonStatus::Up { models: vec![] };
        w.reconcile_pull_status();
        assert_eq!(w.pull, PullStatus::Idle);
    }

    #[test]
    fn navigating_provider_resets_cached_daemon_probe() {
        let mut w = WizardState::new();
        w.daemon = DaemonStatus::Up {
            models: vec!["x".into()],
        };
        w.pull = PullStatus::Done;
        w.navigate_provider(1);
        assert_eq!(w.daemon, DaemonStatus::Unchecked);
        assert_eq!(w.pull, PullStatus::Idle);
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
