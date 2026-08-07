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
    out.push_str("\n[agent]\n");
    out.push_str("max_turns = 40\n");
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

// ---------- Ollama probe + model pull ----------
//
// `provider_base_url` in the wizard's templates points at the
// OpenAI-compat shim (`http://localhost:11434/v1`). Native
// Ollama endpoints (`/api/tags`, `/api/pull`) live one path
// segment up, so strip the trailing `/v1` before composing
// requests.

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OllamaProbe {
    Down { reason: String },
    Up { models: Vec<String> },
}

#[derive(Clone, Debug, PartialEq)]
pub enum PullEvent {
    Phase(String),
    Progress {
        phase: String,
        completed: u64,
        total: u64,
    },
    Done,
    Error(String),
}

pub fn ollama_api_base(provider_base_url: &str) -> String {
    let trimmed = provider_base_url.trim_end_matches('/');
    trimmed.strip_suffix("/v1").unwrap_or(trimmed).to_string()
}

pub fn has_pulled_model(probe: &OllamaProbe, model: &str) -> bool {
    match probe {
        OllamaProbe::Up { models } => models.iter().any(|m| m == model),
        OllamaProbe::Down { .. } => false,
    }
}

pub async fn probe_ollama(provider_base_url: &str, timeout: std::time::Duration) -> OllamaProbe {
    #[derive(serde::Deserialize)]
    struct TagsResp {
        models: Vec<TagEntry>,
    }
    #[derive(serde::Deserialize)]
    struct TagEntry {
        name: String,
    }

    let base = ollama_api_base(provider_base_url);
    let url = format!("{}/api/tags", base);
    let client = match reqwest::Client::builder().timeout(timeout).build() {
        Ok(c) => c,
        Err(e) => {
            return OllamaProbe::Down {
                reason: format!("client build: {}", e),
            };
        }
    };
    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            return OllamaProbe::Down {
                reason: friendly_probe_error(&e),
            };
        }
    };
    if !resp.status().is_success() {
        return OllamaProbe::Down {
            reason: format!("HTTP {}", resp.status()),
        };
    }
    match resp.json::<TagsResp>().await {
        Ok(body) => OllamaProbe::Up {
            models: body.models.into_iter().map(|m| m.name).collect(),
        },
        Err(e) => OllamaProbe::Down {
            reason: format!("parse /api/tags: {}", e),
        },
    }
}

fn friendly_probe_error(e: &reqwest::Error) -> String {
    if e.is_connect() {
        "could not connect (is `ollama serve` running?)".into()
    } else if e.is_timeout() {
        "request timed out".into()
    } else {
        e.to_string()
    }
}

pub fn parse_pull_chunk(line: &str) -> Option<PullEvent> {
    #[derive(serde::Deserialize)]
    struct Chunk {
        #[serde(default)]
        status: String,
        #[serde(default)]
        error: Option<String>,
        #[serde(default)]
        total: Option<u64>,
        #[serde(default)]
        completed: Option<u64>,
    }
    let chunk: Chunk = serde_json::from_str(line.trim()).ok()?;
    if let Some(err) = chunk.error {
        return Some(PullEvent::Error(err));
    }
    if chunk.status == "success" {
        return Some(PullEvent::Done);
    }
    match (chunk.total, chunk.completed) {
        (Some(t), Some(c)) if t > 0 => Some(PullEvent::Progress {
            phase: chunk.status,
            completed: c,
            total: t,
        }),
        _ => Some(PullEvent::Phase(chunk.status)),
    }
}

/// Stream a model pull, emitting one `PullEvent` per JSON line
/// the daemon writes. Returns `Ok` when the daemon emits
/// `status:"success"`; returns `Err` (and stops calling
/// `on_event`) on transport failure or an explicit error chunk.
pub async fn pull_model(
    provider_base_url: &str,
    model: &str,
    mut on_event: impl FnMut(PullEvent),
) -> Result<(), String> {
    use futures::StreamExt;
    let base = ollama_api_base(provider_base_url);
    let url = format!("{}/api/pull", base);
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("client build: {}", e))?;
    let resp = client
        .post(&url)
        .json(&serde_json::json!({"model": model, "stream": true}))
        .send()
        .await
        .map_err(|e| friendly_probe_error(&e))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("HTTP {} — {}", status, body.trim()));
    }
    let mut stream = resp.bytes_stream();
    let mut buf = Vec::<u8>::new();
    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|e| format!("read: {}", e))?;
        buf.extend_from_slice(&bytes);
        while let Some(nl) = buf.iter().position(|&b| b == b'\n') {
            let mut line = buf.drain(..=nl).collect::<Vec<u8>>();
            line.pop();
            let s = String::from_utf8_lossy(&line);
            if s.trim().is_empty() {
                continue;
            }
            let Some(ev) = parse_pull_chunk(&s) else {
                continue;
            };
            let stop = match &ev {
                PullEvent::Done => true,
                PullEvent::Error(msg) => {
                    let msg = msg.clone();
                    on_event(ev);
                    return Err(msg);
                }
                _ => false,
            };
            on_event(ev);
            if stop {
                return Ok(());
            }
        }
    }
    Err("pull stream ended before success".into())
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
    fn render_toml_includes_agent_defaults_block() {
        let body = render_toml(WizardProvider::Ollama, "");
        assert!(body.contains("[agent]"), "missing [agent] block:\n{}", body);
        assert!(
            body.contains("max_turns = 40"),
            "missing max_turns default:\n{}",
            body
        );
    }

    #[test]
    fn render_toml_is_loadable_as_config() {
        let body = render_toml(WizardProvider::Ollama, "");
        let parsed: toml::Value = toml::from_str(&body).expect("must parse as TOML");
        assert_eq!(
            parsed.get("default_provider").and_then(|v| v.as_str()),
            Some("ollama")
        );
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

    #[test]
    fn ollama_api_base_strips_v1_suffix() {
        assert_eq!(
            ollama_api_base("http://localhost:11434/v1"),
            "http://localhost:11434"
        );
    }

    #[test]
    fn ollama_api_base_tolerates_trailing_slash() {
        assert_eq!(
            ollama_api_base("http://localhost:11434/v1/"),
            "http://localhost:11434"
        );
    }

    #[test]
    fn ollama_api_base_keeps_url_without_v1() {
        assert_eq!(
            ollama_api_base("http://10.0.0.5:11434"),
            "http://10.0.0.5:11434"
        );
    }

    #[test]
    fn parse_pull_chunk_recognizes_phase_only_line() {
        let ev = parse_pull_chunk(r#"{"status":"pulling manifest"}"#).unwrap();
        assert_eq!(ev, PullEvent::Phase("pulling manifest".into()));
    }

    #[test]
    fn parse_pull_chunk_extracts_progress_bytes() {
        let line =
            r#"{"status":"downloading","digest":"sha256:abc","total":4682766080,"completed":1024}"#;
        let ev = parse_pull_chunk(line).unwrap();
        assert_eq!(
            ev,
            PullEvent::Progress {
                phase: "downloading".into(),
                completed: 1024,
                total: 4682766080,
            }
        );
    }

    #[test]
    fn parse_pull_chunk_treats_zero_total_as_phase_only() {
        let line = r#"{"status":"verifying","total":0,"completed":0}"#;
        let ev = parse_pull_chunk(line).unwrap();
        assert_eq!(ev, PullEvent::Phase("verifying".into()));
    }

    #[test]
    fn parse_pull_chunk_maps_success_to_done() {
        let ev = parse_pull_chunk(r#"{"status":"success"}"#).unwrap();
        assert_eq!(ev, PullEvent::Done);
    }

    #[test]
    fn parse_pull_chunk_surfaces_error_field() {
        let ev = parse_pull_chunk(r#"{"status":"","error":"model not found"}"#).unwrap();
        assert_eq!(ev, PullEvent::Error("model not found".into()));
    }

    #[test]
    fn parse_pull_chunk_drops_garbage() {
        assert!(parse_pull_chunk("not json").is_none());
    }

    #[test]
    fn has_pulled_model_matches_exact_tag() {
        let probe = OllamaProbe::Up {
            models: vec!["qwen2.5-coder:7b".into(), "llama3:8b".into()],
        };
        assert!(has_pulled_model(&probe, "qwen2.5-coder:7b"));
        assert!(!has_pulled_model(&probe, "qwen2.5-coder:14b"));
    }

    #[test]
    fn has_pulled_model_false_when_daemon_down() {
        let probe = OllamaProbe::Down { reason: "x".into() };
        assert!(!has_pulled_model(&probe, "qwen2.5-coder:7b"));
    }
}
