//! Point `config.toml` at the subscription after a successful login.
//!
//! Signing in and being told "now go hand-edit a TOML file" is a bad
//! ending, and hand-writing the block is error-prone in exactly three
//! ways this module removes: a missing `base_url`, a `default_model`
//! the subscription doesn't serve, and a `default_provider` still
//! pointing somewhere else.
//!
//! # What it will and won't do
//!
//! - Repairs a provider block already using `kind = "openai-chatgpt"`.
//! - Adds one if there is none, and points `default_provider` at it.
//! - Never edits, renames or removes any *other* provider block, so
//!   switching back to an API-key provider is a one-line change to
//!   `default_provider`.
//! - Never overwrites a `default_model` the user chose, even when the
//!   subscription doesn't serve it. It says so instead — silently
//!   changing which model somebody's agent runs is not a repair.
//!
//! Editing goes through `toml_edit` so comments, key order and
//! whitespace survive. A user's config is a document, not a value tree.

use std::path::Path;

use toml_edit::{DocumentMut, Item, Table, value};

use crate::error::{AgentError, Result};

/// Provider block name used when adding one from scratch.
pub const DEFAULT_PROVIDER_NAME: &str = "chatgpt";

/// The `kind` this module manages.
pub const CHATGPT_KIND: &str = "openai-chatgpt";

/// One change applied to the config, for reporting back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// A `[providers.<name>]` block was created.
    AddedProvider(String),
    /// `default_provider` was repointed. Carries the previous value.
    SwitchedDefault { from: Option<String>, to: String },
    /// A key was set on the provider block.
    SetKey {
        provider: String,
        key: String,
        value: String,
    },
    /// Something is wrong but was left alone deliberately.
    Warned(String),
}

impl std::fmt::Display for Change {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AddedProvider(name) => write!(f, "added [providers.{name}]"),
            Self::SwitchedDefault {
                from: Some(from),
                to,
            } => {
                write!(f, "default_provider = \"{to}\"  (was \"{from}\")")
            }
            Self::SwitchedDefault { from: None, to } => {
                write!(f, "default_provider = \"{to}\"")
            }
            Self::SetKey {
                provider,
                key,
                value,
            } => write!(f, "providers.{provider}.{key} = \"{value}\""),
            Self::Warned(message) => write!(f, "! {message}"),
        }
    }
}

/// Apply the subscription settings to a config document.
///
/// Pure over the TOML text: takes the current document and the model
/// slugs the subscription serves, returns the new text plus what
/// changed. All the interesting decisions live here so they can be
/// tested without a filesystem or a network.
///
/// `served_models` may be empty when the catalogue could not be
/// fetched; the model is then left alone rather than guessed.
pub fn apply(config_toml: &str, served_models: &[String]) -> Result<(String, Vec<Change>)> {
    let mut doc: DocumentMut = config_toml
        .parse()
        .map_err(|e| AgentError::Config(format!("config.toml is not valid TOML: {e}")))?;
    let mut changes = Vec::new();

    let existing = find_chatgpt_provider(&doc);
    let name = match existing {
        Some(name) => name,
        None => {
            let name = unique_provider_name(&doc);
            ensure_providers_table(&mut doc);
            let mut table = Table::new();
            table["kind"] = value(CHATGPT_KIND);
            doc["providers"][&name] = Item::Table(table);
            changes.push(Change::AddedProvider(name.clone()));
            name
        }
    };

    // base_url: fill in when absent. `resolved_base_url` would default
    // it anyway, but writing it makes the config self-describing.
    let block = &mut doc["providers"][&name];
    if block
        .get("base_url")
        .and_then(|v| v.as_str())
        .is_none_or(|s| s.trim().is_empty())
    {
        block["base_url"] = value(crate::auth::CHATGPT_BASE_URL);
        changes.push(Change::SetKey {
            provider: name.clone(),
            key: "base_url".into(),
            value: crate::auth::CHATGPT_BASE_URL.to_string(),
        });
    }

    // default_model: set when absent, verify when present.
    let current_model = block
        .get("default_model")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    match (current_model, served_models.first()) {
        (None, Some(preferred)) => {
            doc["providers"][&name]["default_model"] = value(preferred);
            changes.push(Change::SetKey {
                provider: name.clone(),
                key: "default_model".into(),
                value: preferred.clone(),
            });
        }
        (Some(current), Some(_)) if !served_models.contains(&current) => {
            changes.push(Change::Warned(format!(
                "providers.{name}.default_model is \"{current}\", which this subscription \
                 does not serve. Available: {}. Left unchanged — edit it yourself, or \
                 delete the line and re-run `oli login`.",
                served_models.join(", ")
            )));
        }
        (None, None) => changes.push(Change::Warned(format!(
            "could not fetch the model list, so providers.{name}.default_model was not \
             set. Run `oli login` again, or set it by hand."
        ))),
        _ => {}
    }

    // default_provider: point at this block.
    let previous = doc
        .get("default_provider")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    if previous.as_deref() != Some(name.as_str()) {
        doc["default_provider"] = value(&name);
        changes.push(Change::SwitchedDefault {
            from: previous,
            to: name.clone(),
        });
    }

    Ok((doc.to_string(), changes))
}

/// Name of the first provider block using [`CHATGPT_KIND`], preferring
/// the one `default_provider` already names.
fn find_chatgpt_provider(doc: &DocumentMut) -> Option<String> {
    let providers = doc.get("providers")?.as_table_like()?;
    let is_chatgpt = |item: &Item| {
        item.get("kind")
            .and_then(|k| k.as_str())
            .is_some_and(|k| k == CHATGPT_KIND)
    };

    // If the active provider is already the right kind, repair that one
    // rather than adding a second.
    if let Some(active) = doc.get("default_provider").and_then(|v| v.as_str())
        && let Some(item) = providers.get(active)
        && is_chatgpt(item)
    {
        return Some(active.to_string());
    }

    providers
        .iter()
        .find(|(_, item)| is_chatgpt(item))
        .map(|(name, _)| name.to_string())
}

/// A provider name that isn't taken. Avoids clobbering an unrelated
/// block that happens to be called `chatgpt`.
fn unique_provider_name(doc: &DocumentMut) -> String {
    let taken = |name: &str| {
        doc.get("providers")
            .and_then(|p| p.as_table_like())
            .is_some_and(|t| t.contains_key(name))
    };
    if !taken(DEFAULT_PROVIDER_NAME) {
        return DEFAULT_PROVIDER_NAME.to_string();
    }
    (2..)
        .map(|n| format!("{DEFAULT_PROVIDER_NAME}{n}"))
        .find(|name| !taken(name))
        .unwrap_or_else(|| DEFAULT_PROVIDER_NAME.to_string())
}

/// Make sure `[providers]` exists and is a table we can insert into.
fn ensure_providers_table(doc: &mut DocumentMut) {
    if !doc.get("providers").is_some_and(|p| p.is_table_like()) {
        let mut table = Table::new();
        // Implicit so it renders as `[providers.chatgpt]` rather than
        // an empty `[providers]` header followed by the child.
        table.set_implicit(true);
        doc["providers"] = Item::Table(table);
    }
}

/// Read, [`apply`], and write back — with a `.bak` alongside.
///
/// Returns the changes made. Creates the file if it doesn't exist yet,
/// so `oli login` works on a machine with no config at all.
pub fn apply_to_file(path: &Path, served_models: &[String]) -> Result<Vec<Change>> {
    let original = match std::fs::read_to_string(path) {
        Ok(body) => body,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(AgentError::Config(format!(
                "cannot read {}: {e}",
                path.display()
            )));
        }
    };

    let (updated, changes) = apply(&original, served_models)?;
    if changes.iter().all(|c| matches!(c, Change::Warned(_))) {
        return Ok(changes);
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Config(format!("cannot create {}: {e}", parent.display())))?;
    }
    // Keep a copy before rewriting something the user maintains by hand.
    if !original.is_empty() {
        let backup = path.with_extension("toml.bak");
        std::fs::write(&backup, &original)
            .map_err(|e| AgentError::Config(format!("cannot write {}: {e}", backup.display())))?;
    }
    std::fs::write(path, updated)
        .map_err(|e| AgentError::Config(format!("cannot write {}: {e}", path.display())))?;

    Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODELS: [&str; 2] = ["gpt-5.6-terra", "gpt-5.5"];

    fn models() -> Vec<String> {
        MODELS.iter().map(|s| s.to_string()).collect()
    }

    fn apply_str(input: &str) -> (String, Vec<Change>) {
        apply(input, &models()).unwrap()
    }

    #[test]
    fn repairs_the_exact_config_a_user_would_hand_write() {
        // A missing base_url used to fail to parse with serde's
        // `missing field \`base_url\``, naming no provider.
        let (out, changes) = apply_str(
            r#"default_provider = "codex"

[providers.codex]
kind          = "openai-chatgpt"
default_model = "gpt-4o"

[agent]
max_turns = 40
"#,
        );

        assert!(out.contains(r#"base_url = "https://chatgpt.com/backend-api/codex""#));
        // The bad model is flagged, not silently rewritten.
        assert!(out.contains(r#"default_model = "gpt-4o""#));
        assert!(changes.iter().any(|c| matches!(c, Change::Warned(m)
                if m.contains("gpt-4o") && m.contains("gpt-5.6-terra"))));
        // Already the default, so it isn't touched.
        assert!(
            !changes
                .iter()
                .any(|c| matches!(c, Change::SwitchedDefault { .. }))
        );
    }

    #[test]
    fn preserves_comments_key_order_and_unrelated_sections() {
        let input = r#"# my oli config
default_provider = "codex"

[providers.codex]
kind = "openai-chatgpt"   # subscription

[agent]
max_turns = 40

[policy]
auto_allow = ["Read"]
"#;
        let (out, _) = apply_str(input);
        assert!(out.contains("# my oli config"), "{out}");
        assert!(out.contains("# subscription"), "{out}");
        assert!(out.contains("max_turns = 40"), "{out}");
        assert!(out.contains(r#"auto_allow = ["Read"]"#), "{out}");
    }

    #[test]
    fn adds_a_block_and_switches_the_default_when_none_exists() {
        let (out, changes) = apply_str(
            r#"default_provider = "openrouter"

[providers.openrouter]
kind        = "openai-compat"
base_url    = "https://openrouter.ai/api/v1"
api_key_env = "OPENROUTER_API_KEY"
"#,
        );

        assert!(out.contains("[providers.chatgpt]"), "{out}");
        assert!(out.contains(r#"default_provider = "chatgpt""#), "{out}");
        assert!(out.contains(r#"default_model = "gpt-5.6-terra""#), "{out}");
        assert!(changes.contains(&Change::AddedProvider("chatgpt".into())));
        assert!(changes.contains(&Change::SwitchedDefault {
            from: Some("openrouter".into()),
            to: "chatgpt".into()
        }));
    }

    #[test]
    fn the_previous_provider_block_survives_intact() {
        // Switching back must be a one-line edit, not a re-setup.
        let (out, _) = apply_str(
            r#"default_provider = "openrouter"

[providers.openrouter]
kind        = "openai-compat"
base_url    = "https://openrouter.ai/api/v1"
api_key_env = "OPENROUTER_API_KEY"
"#,
        );
        assert!(out.contains("[providers.openrouter]"), "{out}");
        assert!(
            out.contains(r#"api_key_env = "OPENROUTER_API_KEY""#),
            "{out}"
        );
        assert!(out.contains(r#"kind        = "openai-compat""#), "{out}");
    }

    #[test]
    fn works_from_an_empty_config() {
        let (out, changes) = apply_str("");
        assert!(out.contains("[providers.chatgpt]"), "{out}");
        assert!(out.contains(r#"kind = "openai-chatgpt""#), "{out}");
        assert!(out.contains(r#"default_provider = "chatgpt""#), "{out}");
        assert!(changes.contains(&Change::AddedProvider("chatgpt".into())));

        // And the result must be loadable by the real parser.
        let cfg = crate::config::Config::from_str(&out).unwrap();
        assert_eq!(cfg.default_provider, "chatgpt");
        assert_eq!(cfg.providers["chatgpt"].kind, "openai-chatgpt");
    }

    #[test]
    fn does_not_clobber_an_unrelated_block_named_chatgpt() {
        let (out, changes) = apply_str(
            r#"default_provider = "x"

[providers.chatgpt]
kind        = "openai-compat"
base_url    = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"

[providers.x]
kind     = "openai-compat"
base_url = "http://localhost:11434/v1"
api_key  = "ollama"
"#,
        );
        assert!(changes.contains(&Change::AddedProvider("chatgpt2".into())));
        // The impostor keeps its settings.
        assert!(out.contains(r#"api_key_env = "OPENAI_API_KEY""#), "{out}");
        assert!(out.contains("[providers.chatgpt2]"), "{out}");
    }

    #[test]
    fn repairs_the_active_block_rather_than_adding_a_second() {
        let (out, changes) = apply_str(
            r#"default_provider = "codex"

[providers.codex]
kind = "openai-chatgpt"

[providers.spare]
kind = "openai-chatgpt"
"#,
        );
        assert!(
            !changes
                .iter()
                .any(|c| matches!(c, Change::AddedProvider(_)))
        );
        // The active one gained a base_url; the spare did not.
        let codex_section = out.split("[providers.spare]").next().unwrap();
        assert!(codex_section.contains("base_url"), "{out}");
    }

    #[test]
    fn adopts_an_inactive_chatgpt_block_instead_of_adding_one() {
        let (out, changes) = apply_str(
            r#"default_provider = "ollama"

[providers.ollama]
kind     = "openai-compat"
base_url = "http://localhost:11434/v1"
api_key  = "ollama"

[providers.codex]
kind = "openai-chatgpt"
"#,
        );
        assert!(
            !changes
                .iter()
                .any(|c| matches!(c, Change::AddedProvider(_)))
        );
        assert!(changes.contains(&Change::SwitchedDefault {
            from: Some("ollama".into()),
            to: "codex".into()
        }));
        assert!(out.contains(r#"default_provider = "codex""#), "{out}");
    }

    #[test]
    fn a_served_model_is_left_alone_without_a_warning() {
        let (out, changes) = apply_str(
            r#"default_provider = "codex"

[providers.codex]
kind          = "openai-chatgpt"
base_url      = "https://chatgpt.com/backend-api/codex"
default_model = "gpt-5.5"
"#,
        );
        assert!(out.contains(r#"default_model = "gpt-5.5""#));
        assert!(!changes.iter().any(|c| matches!(c, Change::Warned(_))));
    }

    #[test]
    fn an_explicit_base_url_is_not_overwritten() {
        // Someone proxying the endpoint has a reason for that value.
        let (out, _) = apply(
            r#"default_provider = "codex"

[providers.codex]
kind     = "openai-chatgpt"
base_url = "http://localhost:8080/proxy"
"#,
            &models(),
        )
        .unwrap();
        assert!(
            out.contains(r#"base_url = "http://localhost:8080/proxy""#),
            "{out}"
        );
        assert!(!out.contains("chatgpt.com"), "{out}");
    }

    #[test]
    fn an_empty_model_list_warns_rather_than_guessing() {
        let (out, changes) = apply("", &[]).unwrap();
        assert!(!out.contains("default_model"), "{out}");
        assert!(
            changes
                .iter()
                .any(|c| matches!(c, Change::Warned(m) if m.contains("could not fetch")))
        );
    }

    #[test]
    fn invalid_toml_is_reported_not_overwritten() {
        let err = apply("this is not [ valid toml", &models())
            .unwrap_err()
            .to_string();
        assert!(err.contains("not valid TOML"), "{err}");
    }

    #[test]
    fn applying_twice_is_idempotent() {
        let (first, _) = apply_str("");
        let (second, changes) = apply_str(&first);
        assert_eq!(first, second);
        assert!(changes.is_empty(), "{changes:?}");
    }

    // ---- File handling -------------------------------------------

    #[test]
    fn writes_the_file_and_leaves_a_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "default_provider = \"codex\"\n\n[providers.codex]\nkind = \"openai-chatgpt\"\n",
        )
        .unwrap();

        apply_to_file(&path, &models()).unwrap();

        assert!(std::fs::read_to_string(&path).unwrap().contains("base_url"));
        let backup = std::fs::read_to_string(dir.path().join("config.toml.bak")).unwrap();
        assert!(
            !backup.contains("base_url"),
            "backup should be the original"
        );
    }

    #[test]
    fn creates_a_config_when_none_exists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("config.toml");

        apply_to_file(&path, &models()).unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("[providers.chatgpt]"));
        // Nothing to back up when there was no file.
        assert!(!dir.path().join("nested").join("config.toml.bak").exists());
    }

    #[test]
    fn a_config_needing_nothing_is_not_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let body = "default_provider = \"codex\"\n\n[providers.codex]\nkind = \"openai-chatgpt\"\nbase_url = \"https://chatgpt.com/backend-api/codex\"\ndefault_model = \"gpt-5.5\"\n";
        std::fs::write(&path, body).unwrap();

        let changes = apply_to_file(&path, &models()).unwrap();

        assert!(changes.is_empty());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
        assert!(!dir.path().join("config.toml.bak").exists());
    }
}
