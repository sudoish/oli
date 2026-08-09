//! TOML config — `~/.config/oli/config.toml` global, optionally
//! layered with a project-local `.oli/config.toml`. The same
//! `Config` is shared across the agent, providers, policy gate,
//! tool registry, plugin loader, and MCP client.
//!
//! Sections (all optional except where noted):
//! - `default_provider` (required) — picks which `[providers.<name>]`
//!   block to instantiate.
//! - `[providers.<name>]` — per-provider config; `kind` selects
//!   the implementation (`anthropic`, `openai-compat`, …).
//! - `[agent]` — per-run knobs: `max_turns`, compaction target.
//! - `[policy]` — allow/deny rules and approval defaults.
//! - `[[caps]]` — model-capability overrides (context window,
//!   native-tools toggle, …) ahead of the hardcoded defaults.
//! - `[[mcp.servers]]` — external MCP servers to dial at startup.
//! - `[tools.subprocess]` — declared external CLI tools to expose.
//! - `[plugins]` — plugin discovery toggles.
//!
//! `Config::load_or_default()` returns a fully-merged config or
//! falls back to env-only defaults when neither file is present;
//! that's what the binary calls at startup.

use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::{AgentError, Result};
use crate::mcp::McpConfig;
use crate::policy::PolicyConfig;

/// Top-level harness config. Modeled progressively as phases land:
/// providers (phase 0), policy (phase 2), plugins / subprocess tools
/// (phase 2/3), per-project overrides (phase 4).
#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    pub default_provider: String,

    #[serde(default)]
    pub default_model: Option<String>,

    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,

    /// Tool execution policy. A missing section runs tools automatically;
    /// `mode = "ask"` enables the granular approval rules.
    #[serde(default)]
    pub policy: PolicyConfig,

    /// External tools registered as subprocesses (MCP-lite). Empty by
    /// default. Each `[[tools.subprocess]]` entry is wrapped in the
    /// `SubprocessTool` adapter at startup.
    #[serde(default)]
    pub tools: ToolsConfig,

    /// Per-model capability overrides. `[[caps]]` entries with a
    /// `prefix` field shadow the hardcoded registry. Useful when running
    /// a custom Ollama-tagged model whose context window or tool-call
    /// support differs from the family default.
    #[serde(default)]
    pub caps: Vec<crate::agent::caps::CapsOverride>,

    /// Top-level agent runtime knobs.
    #[serde(default)]
    pub agent: AgentConfig,

    /// MCP client config — `[mcp.servers.*]` tables. Empty by default;
    /// each entry the user adds becomes a connected server at startup.
    #[serde(default)]
    pub mcp: McpConfig,

    /// `[ui]` section — terminal-UI preferences shared by the TUI and
    /// (where relevant) the line-mode REPL. Optional; defaults match
    /// the pre-W1 behavior.
    #[serde(default)]
    pub ui: UiConfig,
}

/// `[ui]` config block.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct UiConfig {
    /// Which viewport mode the TUI should pick. One of `"auto"`,
    /// `"fullscreen"`, `"inline"`. `"auto"` defers to capability
    /// detection (Phase W2): inline inside buffer-terminals,
    /// fullscreen elsewhere.
    #[serde(default)]
    pub viewport: Option<String>,

    /// Override mouse-capture default. `None` = follow the viewport
    /// and capability defaults (capture in fullscreen, off in
    /// inline / buffer-terminals). `Some(true)` forces capture on,
    /// `Some(false)` forces it off — the latter is useful in a
    /// fullscreen kitty/iTerm2 session when the user wants
    /// terminal-native selection back.
    #[serde(default)]
    pub mouse: Option<bool>,

    /// Override OSC52 clipboard-write default. `None` / `"auto"` =
    /// follow `Capabilities::osc52` (on in kitty / iTerm2 / WezTerm
    /// / ghostty / tmux-capable; off in buffer-terminals + generic
    /// xterm). `"on"` forces the escape; `"off"` always falls back
    /// to the copy-fallback modal. Useful when a host *does* honor
    /// OSC52 but we couldn't detect it, or vice versa.
    #[serde(default)]
    pub osc52: Option<String>,

    /// Named theme for the TUI. One of `"dark"` (default),
    /// `"light"`, `"dimmed"`, or `"auto"`. `"auto"` consults
    /// `$COLORFGBG` and picks `light` on a light-background
    /// terminal, otherwise `dark`. Unknown names fall back to
    /// `dark`.
    #[serde(default)]
    pub theme: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AgentConfig {
    /// Maximum number of model turns per top-level `run` invocation.
    /// Bounds the loop so a flaky model that re-issues the same tool
    /// call on every turn (qwen with the fallback parser, observed
    /// during Phase 3 smoke) can't spin forever. Default 40.
    #[serde(default = "AgentConfig::default_max_turns")]
    pub max_turns: usize,
}

impl AgentConfig {
    fn default_max_turns() -> usize {
        40
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_turns: Self::default_max_turns(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ToolsConfig {
    #[serde(default)]
    pub subprocess: Vec<SubprocessToolConfig>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SubprocessToolConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub description: String,
    /// JSON-Schema describing the tool's args object. Defaults to a
    /// no-args `{type:"object", properties:{}}` so a config entry that
    /// only sets `name`, `command`, `description` still produces a valid
    /// tool spec for the model.
    #[serde(default = "default_parameters")]
    pub parameters: Value,
}

fn default_parameters() -> Value {
    json!({"type": "object", "properties": {}})
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProviderConfig {
    pub kind: String,

    /// API base URL. Optional, because some kinds have exactly one
    /// sensible value and requiring it is just a trap: `openai-chatgpt`
    /// always talks to the subscription endpoint, and `anthropic`
    /// always talks to `api.anthropic.com`. `openai-compat` still
    /// requires it — there is no default that could be right for
    /// Ollama, OpenRouter, vLLM and OpenAI at once.
    ///
    /// Resolve with [`ProviderConfig::resolved_base_url`] rather than
    /// reading this directly.
    #[serde(default)]
    pub base_url: Option<String>,

    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub default_model: Option<String>,

    /// Prompt-caching strategy. Default `None` means no cache hints
    /// are sent (the OpenAI-compat path's pre-Phase-D behavior).
    /// `"anthropic"` makes the OpenAI-compat provider attach
    /// `cache_control: ephemeral` blocks on the system message and
    /// the last tool definition the same way the native Anthropic
    /// provider does — useful for routing through OpenRouter to
    /// Claude where the upstream supports Anthropic-style caching.
    /// Auto-detection layers on top: an OpenRouter base_url plus an
    /// Anthropic-shaped model id is treated as `"anthropic"` even
    /// when the user hasn't set the field. Has no effect on the
    /// native Anthropic provider, which already caches.
    #[serde(default)]
    pub cache: Option<String>,
}

impl ProviderConfig {
    /// The base URL to dial, applying the per-kind default when the
    /// config omits one.
    ///
    /// Errors for kinds that have no meaningful default. The error
    /// names the field and the kind, which is friendlier than serde's
    /// `missing field \`base_url\`` with no indication of which of
    /// several provider blocks is at fault.
    pub fn resolved_base_url(&self, provider_name: &str) -> Result<String> {
        if let Some(url) = self.base_url.as_deref() {
            let trimmed = url.trim();
            if !trimmed.is_empty() {
                return Ok(trimmed.to_string());
            }
        }
        match self.kind.as_str() {
            "openai-chatgpt" => Ok(crate::auth::CHATGPT_BASE_URL.to_string()),
            "anthropic" => Ok(DEFAULT_ANTHROPIC_BASE_URL.to_string()),
            _ => Err(AgentError::Config(format!(
                "provider '{}' (kind '{}') needs a `base_url` — there is no default for \
                 this kind. For example: base_url = \"https://api.openai.com/v1\"",
                provider_name, self.kind
            ))),
        }
    }
}

/// Where the native Anthropic provider points when the config doesn't
/// say. Matches what the README has always shown.
pub const DEFAULT_ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";

impl Config {
    pub fn from_str(s: &str) -> Result<Self> {
        toml::from_str(s).map_err(|e| AgentError::Config(e.to_string()))
    }

    /// Read + parse a single TOML file. The binary uses
    /// `load_or_default` (which layers global + project), but
    /// this single-file form stays public for tooling/tests that
    /// want to point at a specific file.
    #[allow(dead_code)]
    pub fn from_file(path: &Path) -> Result<Self> {
        let body = std::fs::read_to_string(path)?;
        Self::from_str(&body)
    }

    /// Load `~/.config/oli/config.toml` if present, then layer any
    /// project-scoped `.oli/config.toml` over the top. Falls back to
    /// an env-only default if neither file exists.
    ///
    /// Merge semantics (overlay = project, base = global):
    /// - Tables merge per key; overlay wins on scalar leaves.
    /// - Arrays concatenate **with overlay items first** so e.g.
    ///   project-scoped `[[caps]]` shadow global ones in the lookup
    ///   order used by `caps_for_with_overrides`.
    /// - Missing keys in overlay leave base unchanged (api keys stay
    ///   in global, repos stay credential-free).
    pub fn load_or_default() -> Result<Self> {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self::load_layered(&cwd)
    }

    /// Test seam — same as `load_or_default` but accepts an explicit
    /// cwd so tests can run inside a tempdir without polluting the real
    /// `~/.config/oli/config.toml`.
    pub fn load_layered(cwd: &Path) -> Result<Self> {
        let global_str = match default_config_path() {
            Some(p) if p.exists() => Some(std::fs::read_to_string(&p)?),
            _ => None,
        };
        let project_str = match find_project_config(cwd) {
            Some(p) => Some(std::fs::read_to_string(&p)?),
            None => None,
        };

        match (global_str, project_str) {
            (None, None) => Ok(Self::env_default()),
            (Some(g), None) => Self::from_str(&g),
            (None, Some(p)) => Self::from_str(&p),
            (Some(g), Some(p)) => {
                let global_val: toml::Value = toml::from_str(&g)
                    .map_err(|e| AgentError::Config(format!("global config: {}", e)))?;
                let project_val: toml::Value = toml::from_str(&p)
                    .map_err(|e| AgentError::Config(format!("project config: {}", e)))?;
                let merged = merge_toml(global_val, project_val);
                merged
                    .try_into::<Self>()
                    .map_err(|e| AgentError::Config(format!("layered config: {}", e)))
            }
        }
    }

    pub fn env_default() -> Self {
        let base_url = std::env::var("OPENROUTER_BASE_URL")
            .unwrap_or_else(|_| "https://openrouter.ai/api/v1".to_string());
        let mut providers = HashMap::new();
        providers.insert(
            "openrouter".to_string(),
            ProviderConfig {
                kind: "openai-compat".to_string(),
                base_url: Some(base_url),
                api_key: None,
                api_key_env: Some("OPENROUTER_API_KEY".to_string()),
                default_model: Some("anthropic/claude-haiku-4.5".to_string()),
                cache: None,
            },
        );
        Self {
            default_provider: "openrouter".to_string(),
            default_model: None,
            providers,
            policy: PolicyConfig::default(),
            tools: ToolsConfig::default(),
            caps: Vec::new(),
            agent: AgentConfig::default(),
            mcp: McpConfig::default(),
            ui: UiConfig::default(),
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

/// `$XDG_CONFIG_HOME/oli`, falling back to `$HOME/.config/oli`.
/// `None` when neither env var is set. Shared by the config file and
/// the auth-token store so both land in the same directory.
pub fn config_dir() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(dir.join("oli"))
}

pub fn default_config_path() -> Option<PathBuf> {
    Some(config_dir()?.join("config.toml"))
}

/// Walk up from `cwd` looking for `.oli/config.toml`. Returns the
/// nearest one (innermost wins). We do not merge multiple project
/// configs along the walk — repos that nest config get the closest one.
pub fn find_project_config(cwd: &Path) -> Option<PathBuf> {
    let mut p: &Path = cwd;
    loop {
        let candidate = p.join(".oli").join("config.toml");
        if candidate.exists() {
            return Some(candidate);
        }
        match p.parent() {
            Some(parent) if parent != p => p = parent,
            _ => return None,
        }
    }
}

/// Merge two TOML values. Tables merge per key (overlay wins on
/// scalar leaves). Arrays concatenate with overlay items first, so a
/// project-scoped `[[caps]]` entry takes precedence over a global one
/// during prefix lookup. For other type mismatches, overlay wins.
pub(crate) fn merge_toml(base: toml::Value, overlay: toml::Value) -> toml::Value {
    use toml::Value::{Array, Table};
    match (base, overlay) {
        (Table(mut bt), Table(ot)) => {
            for (k, v) in ot {
                let merged = match bt.remove(&k) {
                    Some(bv) => merge_toml(bv, v),
                    None => v,
                };
                bt.insert(k, merged);
            }
            Table(bt)
        }
        (Array(ba), Array(oa)) => {
            let mut combined = oa;
            combined.extend(ba);
            Array(combined)
        }
        (_, v) => v,
    }
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
    fn missing_policy_mode_defaults_to_auto() {
        let cfg = Config::from_str(SAMPLE).unwrap();
        assert_eq!(cfg.policy.mode, crate::policy::PolicyMode::Auto);
    }

    #[test]
    fn policy_mode_can_enable_approvals() {
        let cfg = Config::from_str(&format!("{SAMPLE}\n[policy]\nmode = \"ask\"\n")).unwrap();
        assert_eq!(cfg.policy.mode, crate::policy::PolicyMode::Ask);
    }

    #[test]
    fn invalid_policy_mode_is_rejected() {
        let err = Config::from_str(&format!("{SAMPLE}\n[policy]\nmode = \"sometimes\"\n"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown variant"), "{err}");
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
    fn openai_compat_still_requires_an_explicit_base_url() {
        // There is no default that is right for Ollama, OpenRouter,
        // vLLM and OpenAI at once, so this stays required.
        let cfg = Config::from_str(
            r#"
            default_provider = "x"
            [providers.x]
            kind = "openai-compat"
            "#,
        )
        .unwrap();
        let err = cfg
            .provider("x")
            .unwrap()
            .resolved_base_url("x")
            .unwrap_err()
            .to_string();
        assert!(err.contains("needs a `base_url`"), "{err}");
        assert!(err.contains("'x'"), "{err}");
    }

    #[test]
    fn chatgpt_and_anthropic_kinds_default_their_base_url() {
        // Requiring a value with exactly one correct answer is a trap:
        // a hand-written block that omits it used to fail to parse
        // with no hint about which provider was at fault.
        let cfg = Config::from_str(
            r#"
            default_provider = "codex"
            [providers.codex]
            kind = "openai-chatgpt"
            [providers.claude]
            kind = "anthropic"
            "#,
        )
        .unwrap();
        assert_eq!(
            cfg.provider("codex")
                .unwrap()
                .resolved_base_url("codex")
                .unwrap(),
            "https://chatgpt.com/backend-api/codex"
        );
        assert_eq!(
            cfg.provider("claude")
                .unwrap()
                .resolved_base_url("claude")
                .unwrap(),
            "https://api.anthropic.com"
        );
    }

    #[test]
    fn an_explicit_base_url_still_wins_over_the_default() {
        let cfg = Config::from_str(
            r#"
            default_provider = "codex"
            [providers.codex]
            kind     = "openai-chatgpt"
            base_url = "http://localhost:8080/proxy"
            "#,
        )
        .unwrap();
        assert_eq!(
            cfg.provider("codex")
                .unwrap()
                .resolved_base_url("codex")
                .unwrap(),
            "http://localhost:8080/proxy"
        );
    }

    #[test]
    fn a_blank_base_url_is_treated_as_absent() {
        let cfg = Config::from_str(
            r#"
            default_provider = "codex"
            [providers.codex]
            kind     = "openai-chatgpt"
            base_url = "   "
            "#,
        )
        .unwrap();
        assert_eq!(
            cfg.provider("codex")
                .unwrap()
                .resolved_base_url("codex")
                .unwrap(),
            "https://chatgpt.com/backend-api/codex"
        );
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

    #[test]
    fn merge_toml_overlay_scalar_wins_on_leaf() {
        let base: toml::Value = toml::from_str(r#"k = 1"#).unwrap();
        let over: toml::Value = toml::from_str(r#"k = 2"#).unwrap();
        let merged = merge_toml(base, over);
        assert_eq!(merged.get("k").unwrap().as_integer().unwrap(), 2);
    }

    #[test]
    fn merge_toml_tables_merge_per_key() {
        let base: toml::Value = toml::from_str(
            r#"
            [providers.openrouter]
            kind = "openai-compat"
            base_url = "https://openrouter.ai/api/v1"
            "#,
        )
        .unwrap();
        let over: toml::Value = toml::from_str(
            r#"
            [providers.local]
            kind = "openai-compat"
            base_url = "http://localhost:11434/v1"
            "#,
        )
        .unwrap();
        let merged = merge_toml(base, over);
        let providers = merged.get("providers").unwrap().as_table().unwrap();
        assert!(providers.contains_key("openrouter"));
        assert!(providers.contains_key("local"));
    }

    #[test]
    fn merge_toml_arrays_concat_with_overlay_first() {
        let base: toml::Value = toml::from_str(
            r#"
            [[caps]]
            prefix = "global"
            ctx_window = 8000
            "#,
        )
        .unwrap();
        let over: toml::Value = toml::from_str(
            r#"
            [[caps]]
            prefix = "project"
            ctx_window = 16000
            "#,
        )
        .unwrap();
        let merged = merge_toml(base, over);
        let caps = merged.get("caps").unwrap().as_array().unwrap();
        assert_eq!(caps.len(), 2);
        // Project entry comes first so it wins prefix-lookup ties.
        assert_eq!(caps[0].get("prefix").unwrap().as_str().unwrap(), "project");
        assert_eq!(caps[1].get("prefix").unwrap().as_str().unwrap(), "global");
    }

    #[test]
    fn load_layered_uses_project_config_when_only_project_present() {
        // Test in a tempdir with no global config visible. We can't
        // easily isolate the global config without env override, so
        // the assertion is "project values are present" — the global
        // either exists or doesn't, but project specifics still land.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".oli")).unwrap();
        std::fs::write(
            dir.path().join(".oli").join("config.toml"),
            r#"
default_provider = "ollama"
[providers.ollama]
kind = "openai-compat"
base_url = "http://localhost:11434/v1"
api_key = "ollama"
default_model = "qwen2.5-coder:7b"

[agent]
max_turns = 7
"#,
        )
        .unwrap();
        let cfg = Config::load_layered(dir.path()).unwrap();
        assert_eq!(cfg.default_provider, "ollama");
        assert_eq!(cfg.agent.max_turns, 7);
        assert!(cfg.providers.contains_key("ollama"));
    }

    #[test]
    fn find_project_config_walks_up_parent_dirs() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("a").join("b").join("c");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(root.path().join(".oli")).unwrap();
        std::fs::write(
            root.path().join(".oli").join("config.toml"),
            "default_provider = \"x\"\n[providers.x]\nkind=\"openai-compat\"\nbase_url=\"u\"\n",
        )
        .unwrap();
        let found = find_project_config(&nested).unwrap();
        assert!(found.starts_with(root.path()));
    }

    #[test]
    fn agent_config_section_parses_with_explicit_max_turns() {
        let toml = r#"
default_provider = "x"
[providers.x]
kind = "openai-compat"
base_url = "u"
[agent]
max_turns = 99
"#;
        let cfg = Config::from_str(toml).unwrap();
        assert_eq!(cfg.agent.max_turns, 99);
    }

    #[test]
    fn agent_config_default_max_turns_when_section_missing() {
        let toml = r#"
default_provider = "x"
[providers.x]
kind = "openai-compat"
base_url = "u"
"#;
        let cfg = Config::from_str(toml).unwrap();
        assert_eq!(cfg.agent.max_turns, 40);
    }

    #[test]
    fn remote_workstation_provider_examples_are_valid_configs() {
        for example in [
            include_str!("../examples/remote-workstation/chatgpt.toml"),
            include_str!("../examples/remote-workstation/openrouter.toml"),
            include_str!("../examples/remote-workstation/ollama.toml"),
        ] {
            Config::from_str(example).unwrap();
        }
    }
}
