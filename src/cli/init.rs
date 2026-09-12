//! First-run configuration command.

use std::io::Write;

use crate::error::{AgentError, Result};
use crate::wizard_init::{WizardProvider, config_path, render_toml, save};

/// Inputs to first-run configuration.
#[derive(Debug, Default)]
pub struct Options {
    pub provider: Option<String>,
    pub api_key: Option<String>,
    pub force: bool,
    pub skip_ollama_check: bool,
    pub pull: bool,
}

/// Write starter config, prompting for omitted values when needed.
pub async fn run(options: Options) -> Result<()> {
    let path = config_path().ok_or_else(|| {
        AgentError::Config("could not resolve config path (no $HOME or $XDG_CONFIG_HOME)".into())
    })?;

    let provider = match options.provider.as_deref() {
        Some(name) => WizardProvider::from_name(name).ok_or_else(|| {
            AgentError::Config(format!(
                "unknown --provider `{}` (try ollama, openrouter, anthropic)",
                name
            ))
        })?,
        None => prompt_provider()?,
    };

    let api_key = if provider.needs_api_key() {
        match options.api_key {
            Some(k) if !k.trim().is_empty() => k,
            _ => prompt_api_key(provider)?,
        }
    } else {
        String::new()
    };

    let body = render_toml(provider, &api_key);
    save(&path, &body, options.force).map_err(AgentError::Config)?;

    let mut stdout = std::io::stdout();
    let _ = writeln!(stdout, "wrote {}", path.display());
    let _ = writeln!(stdout, "  provider:      {}", provider.label());
    let _ = writeln!(stdout, "  default_model: {}", provider.default_model());
    if !provider.needs_api_key() {
        let _ = writeln!(stdout, "  (Ollama: api_key field is a placeholder)");
    }

    if matches!(provider, WizardProvider::Ollama) && !options.skip_ollama_check {
        ollama_post_init(provider, options.pull).await?;
    }

    let _ = writeln!(stdout, "Run `oli` to start.");
    Ok(())
}

async fn ollama_post_init(provider: WizardProvider, auto_pull: bool) -> Result<()> {
    use std::time::Duration;

    use crate::wizard_init::{OllamaProbe, PullEvent, has_pulled_model, probe_ollama, pull_model};

    let mut stdout = std::io::stdout();
    let model = provider.default_model();
    let base_url = provider.base_url();

    let _ = writeln!(stdout, "Checking Ollama at {} ...", base_url);
    let _ = stdout.flush();
    let probe = probe_ollama(base_url, Duration::from_secs(2)).await;
    match &probe {
        OllamaProbe::Down { reason } => {
            let _ = writeln!(stdout, "  ⚠ Ollama not reachable: {}", reason);
            let _ = writeln!(
                stdout,
                "    install:  https://ollama.com/download   (then `ollama serve`)"
            );
            let _ = writeln!(stdout, "    once running:  ollama pull {}", model);
            return Ok(());
        }
        OllamaProbe::Up { models } => {
            let _ = writeln!(
                stdout,
                "  ✓ daemon reachable ({} model{} installed)",
                models.len(),
                if models.len() == 1 { "" } else { "s" }
            );
        }
    }

    if has_pulled_model(&probe, model) {
        let _ = writeln!(stdout, "  ✓ {} already pulled", model);
        return Ok(());
    }

    if !auto_pull {
        let _ = writeln!(
            stdout,
            "  ⚠ {} is not pulled. Run `oli init --provider ollama --pull --force`",
            model
        );
        let _ = writeln!(stdout, "    or  `ollama pull {}` to download it.", model);
        return Ok(());
    }

    let _ = writeln!(
        stdout,
        "Pulling {} (this can take a few minutes) ...",
        model
    );
    let _ = stdout.flush();
    let mut last_pct: i32 = -1;
    pull_model(base_url, model, |ev| match ev {
        PullEvent::Phase(p) => {
            let _ = writeln!(std::io::stdout(), "  · {}", p);
        }
        PullEvent::Progress {
            phase,
            completed,
            total,
        } => {
            let pct = ((completed as f64 / total as f64) * 100.0) as i32;
            if pct != last_pct {
                last_pct = pct;
                let _ = writeln!(
                    std::io::stdout(),
                    "  · {} {}% ({} / {})",
                    phase,
                    pct,
                    human_bytes(completed),
                    human_bytes(total)
                );
            }
        }
        PullEvent::Done => {
            let _ = writeln!(std::io::stdout(), "  ✓ pulled {}", model);
        }
        PullEvent::Error(e) => {
            let _ = writeln!(std::io::stdout(), "  ✗ {}", e);
        }
    })
    .await
    .map_err(AgentError::Config)?;
    Ok(())
}

fn human_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut size = n as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    format!("{:.1}{}", size, UNITS[unit])
}

fn prompt_provider() -> Result<WizardProvider> {
    use std::io::BufRead;

    let mut stdout = std::io::stdout();
    let _ = writeln!(stdout, "Pick a provider:");
    for (i, provider) in WizardProvider::all().iter().enumerate() {
        let _ = writeln!(stdout, "  [{}] {}", i + 1, provider.label());
    }
    let _ = write!(stdout, "Choice [1-3]: ");
    let _ = stdout.flush();
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| AgentError::Config(format!("stdin read failed: {}", e)))?;
    let idx: usize = line
        .trim()
        .parse()
        .map_err(|_| AgentError::Config(format!("`{}` is not 1-3", line.trim())))?;
    WizardProvider::all()
        .get(idx.wrapping_sub(1))
        .copied()
        .ok_or_else(|| AgentError::Config(format!("choice {} out of range (1-3)", idx)))
}

fn prompt_api_key(provider: WizardProvider) -> Result<String> {
    use std::io::{BufRead, IsTerminal};

    let stdin = std::io::stdin();
    let key = if stdin.is_terminal() {
        rpassword::prompt_password(format!("API key for {}: ", provider.label()))
            .map_err(|e| AgentError::Config(format!("stdin read failed: {e}")))?
    } else {
        let mut stdout = std::io::stdout();
        let _ = write!(stdout, "API key for {}: ", provider.label());
        let _ = stdout.flush();
        let mut line = String::new();
        stdin
            .lock()
            .read_line(&mut line)
            .map_err(|e| AgentError::Config(format!("stdin read failed: {e}")))?;
        line
    };
    let key = key.trim().to_string();
    if key.is_empty() {
        return Err(AgentError::Config(
            "api key is required for paid providers".into(),
        ));
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_counts_use_binary_units() {
        assert_eq!(human_bytes(0), "0.0B");
        assert_eq!(human_bytes(1024), "1.0KB");
        assert_eq!(human_bytes(1024 * 1024), "1.0MB");
    }
}
