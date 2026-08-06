//! Terminal capabilities, probed once at TUI startup.
//!
//! Phase W2 fills the env-var-based heuristics: which host are we
//! running inside, does it support truecolor, OSC52, kitty keyboard,
//! and image protocols. Phase W5 will add the optional (gated)
//! DA1/DA2 query path for known-good terminal families.
//!
//! The struct is plain data — every field is set by `Capabilities::detect`
//! at startup; downstream code only reads from it. The `EnvSnapshot`
//! shim makes the probe testable: production code calls `detect()`
//! (which snapshots `std::env`); unit tests build an `EnvSnapshot` by
//! hand and call `detect_with_env(&snap)`.

use std::collections::HashMap;

/// What graphics protocol (if any) the host terminal supports for
/// inline image rendering. Phase Y3 consumes this to pick a
/// rendering strategy; W2 just classifies the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum GraphicsKind {
    Kitty,
    ITerm2,
    Sixel,
    /// Unicode half-block fallback. Universally available but low-fi.
    HalfBlock,
    #[default]
    None,
}

/// Snapshot of capability-relevant environment variables. Owning
/// the lookups behind a struct keeps `detect` pure and lets tests
/// exercise specific host configurations without poisoning the
/// real process environment.
#[derive(Clone, Debug, Default)]
pub struct EnvSnapshot {
    vars: HashMap<String, String>,
}

impl EnvSnapshot {
    /// Capture the current process environment. The fast path at
    /// startup.
    pub fn from_process() -> Self {
        let vars = std::env::vars().collect();
        Self { vars }
    }

    /// Test helper: build a snapshot from a list of `(key, value)`
    /// pairs. Missing keys are absent (not empty), matching real
    /// env semantics.
    #[cfg(test)]
    pub fn from_pairs<I, K, V>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let vars = pairs
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
        Self { vars }
    }

    fn get(&self, key: &str) -> Option<&str> {
        self.vars.get(key).map(String::as_str)
    }

    /// True iff the env var is set to a non-empty value.
    fn is_set(&self, key: &str) -> bool {
        self.get(key).map(|v| !v.is_empty()).unwrap_or(false)
    }
}

/// Resolved terminal capabilities. All fields are immutable post-
/// `detect`. Phase W2 fills the env-derived fields; later phases
/// may add a refresh path for runtime-toggle features (mouse).
#[derive(Clone, Debug)]
pub struct Capabilities {
    /// `oli` is running inside another app's terminal buffer
    /// (Neovim `:terminal`, VSCode integrated terminal, Helix
    /// `:sh`, Zellij, Emacs term). Drives inline-by-default and
    /// disables every probe that has been observed to hang in
    /// these hosts.
    pub is_buffer_terminal: bool,
    /// Host supports truecolor (24-bit RGB). False ⇒ stick to the
    /// 256-color palette.
    pub truecolor: bool,
    /// Host honors the kitty keyboard protocol. We push the
    /// enhancement flags only when this is true.
    pub kitty_keyboard: bool,
    /// Host honors OSC52 clipboard writes. When false, `/copy N`
    /// (Phase W4) opens a fallback modal rather than silently
    /// writing the escape into the void.
    pub osc52: bool,
    /// Best graphics protocol the host supports.
    pub graphics: GraphicsKind,
    /// Mouse capture is *allowed*. Whether it's actually enabled
    /// at startup depends on the viewport mode and `[ui].mouse`
    /// (Phase W3).
    pub mouse: bool,
    /// Focus events (`CSI I` / `CSI O`) are safe to enable.
    pub focus_events: bool,
    /// It is safe to issue blocking terminal queries (DA1/DA2,
    /// cursor-position, `\e]11;?\a`). False inside buffer-terminals
    /// — Neovim's libvterm-backed `:terminal` has been observed to
    /// not reply, hanging the probe. Phase W5 gates every query
    /// behind this flag.
    pub query_ok: bool,
    /// Human-readable host identifier, used by `/diagnostics` and
    /// startup logging. Not for behavioral branching.
    pub host: String,
}

impl Capabilities {
    /// Detect capabilities from the current process environment.
    pub fn detect() -> Self {
        Self::detect_with_env(&EnvSnapshot::from_process())
    }

    /// Pure-function probe — every fact derived from the snapshot.
    /// Use this directly from tests; production code calls
    /// `detect()`.
    pub fn detect_with_env(env: &EnvSnapshot) -> Self {
        let host = identify_host(env);
        let is_buffer_terminal = matches!(
            host.as_str(),
            "neovim:terminal" | "vscode" | "emacs:term" | "jetbrains"
        );

        // Truecolor: explicit COLORTERM marker, or a known-truecolor
        // terminal family. ghostty / kitty / wezterm / alacritty /
        // foot have shipped truecolor for years.
        let colorterm = env.get("COLORTERM").unwrap_or("");
        let truecolor = colorterm.eq_ignore_ascii_case("truecolor")
            || colorterm.eq_ignore_ascii_case("24bit")
            || is_known_truecolor_term(env.get("TERM").unwrap_or(""));

        // Kitty keyboard protocol: only push the enhancement flags
        // when the host advertises kitty-style behavior. Pushing
        // them inside vim/emacs/foreign hosts produces escape
        // garbage in the input box.
        let term = env.get("TERM").unwrap_or("");
        let term_program = env.get("TERM_PROGRAM").unwrap_or("");
        let kitty_keyboard = !is_buffer_terminal
            && (term == "xterm-kitty"
                || term_program == "ghostty"
                || term.starts_with("foot")
                || term_program == "WezTerm");

        // OSC52: kitty / iTerm2 / WezTerm / ghostty land it cleanly.
        // tmux requires `set-clipboard on` — we treat it as on iff
        // the user is in tmux *and* the outer terminal is one of
        // the OSC52-friendly ones (best-effort; the user can still
        // opt out via [ui].osc52 = "off" once W4 ships).
        let osc52 = !is_buffer_terminal
            && (term == "xterm-kitty"
                || term_program == "iTerm.app"
                || term_program == "WezTerm"
                || term_program == "ghostty"
                || term.starts_with("foot")
                || (env.is_set("TMUX") && term_program != "vscode"));

        // Graphics: pick the richest protocol the host advertises.
        // Buffer-terminals are forced down to HalfBlock (which is
        // unicode-only, always renderable) so Phase Y3 doesn't try
        // Kitty/Sixel inside a host that won't pass them through.
        let graphics = if is_buffer_terminal {
            GraphicsKind::HalfBlock
        } else if term == "xterm-kitty" || term_program == "ghostty" {
            GraphicsKind::Kitty
        } else if term_program == "iTerm.app" || term_program == "WezTerm" {
            GraphicsKind::ITerm2
        } else if term.contains("sixel") {
            GraphicsKind::Sixel
        } else {
            GraphicsKind::HalfBlock
        };

        // Mouse: capture is allowed (= safe to enable) unless we're
        // in a host that has its own mouse semantics. The viewport
        // / config layer decides whether to actually enable it.
        let mouse = !is_buffer_terminal;
        // Focus events: same gate; buffer-terminals don't forward
        // them cleanly.
        let focus_events = !is_buffer_terminal;
        // Terminal queries: skip inside buffer-terminals; Phase W5
        // adds a 100 ms timeout on top of this gate.
        let query_ok = !is_buffer_terminal;

        Self {
            is_buffer_terminal,
            truecolor,
            kitty_keyboard,
            osc52,
            graphics,
            mouse,
            focus_events,
            query_ok,
            host,
        }
    }

    /// The viewport the auto-mode resolver should pick when the
    /// user hasn't set a flag or a config value.
    pub fn auto_viewport(&self) -> super::terminal::ViewportMode {
        if self.is_buffer_terminal {
            super::terminal::ViewportMode::Inline
        } else {
            super::terminal::ViewportMode::Fullscreen
        }
    }
}

/// Whether the host supports OSC52 clipboard writes for `/copy N`.
/// Priority order:
///
/// 1. Explicit config override (`[ui].osc52 = "on" | "off"`).
/// 2. `"auto"` / unset ⇒ defer to `caps.osc52`.
///
/// Unknown values fall through to auto rather than erroring; a
/// typo'd config still produces a working clipboard path.
pub fn resolve_osc52(config: Option<&str>, caps_osc52: bool) -> bool {
    match config.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("on") | Some("true") | Some("yes") => true,
        Some("off") | Some("false") | Some("no") => false,
        _ => caps_osc52,
    }
}

/// Identify the host environment from env vars. The string is
/// stable and intended for logging / `/diagnostics`; downstream
/// behavioral checks should read the typed fields on `Capabilities`,
/// not parse this back out.
fn identify_host(env: &EnvSnapshot) -> String {
    if env.is_set("NVIM") || env.is_set("NVIM_LISTEN_ADDRESS") {
        return "neovim:terminal".into();
    }
    if env.get("TERM_PROGRAM") == Some("vscode") || env.is_set("VSCODE_INJECTION") {
        return "vscode".into();
    }
    if env.is_set("INSIDE_EMACS") {
        return "emacs:term".into();
    }
    if env.is_set("TERMINAL_EMULATOR") && env.get("TERMINAL_EMULATOR") == Some("JetBrains-JediTerm")
    {
        return "jetbrains".into();
    }
    // Zellij is a passthrough multiplexer — its `:` keystroke
    // model doesn't fight oli's, and it forwards mouse/OSC52
    // cleanly. Don't mark it as a buffer-terminal even though
    // ZELLIJ is set; let it inherit the underlying terminal's
    // behavior.
    if let Some(p) = env.get("TERM_PROGRAM") {
        if !p.is_empty() {
            return p.to_string();
        }
    }
    if let Some(t) = env.get("TERM") {
        if !t.is_empty() {
            return t.to_string();
        }
    }
    "unknown".into()
}

fn is_known_truecolor_term(term: &str) -> bool {
    matches!(
        term,
        "xterm-kitty" | "alacritty" | "wezterm" | "foot" | "foot-extra" | "ghostty"
    ) || term.starts_with("tmux-256color")
        || term.starts_with("screen-256color")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(pairs: &[(&str, &str)]) -> EnvSnapshot {
        EnvSnapshot::from_pairs(pairs.iter().map(|&(k, v)| (k, v)))
    }

    #[test]
    fn neovim_terminal_is_buffer_terminal_with_safe_defaults() {
        // Inside `:terminal` Neovim sets NVIM and NVIM_LISTEN_ADDRESS.
        // Even though TERM may say xterm-kitty (because the outer
        // terminal is kitty), the buffer-terminal flag wins — kitty
        // keyboard / OSC52 / DA1 queries don't reach the outer host.
        let env = snap(&[
            ("NVIM", "/tmp/nvim.1234.0"),
            ("NVIM_LISTEN_ADDRESS", "/tmp/nvim.1234.0"),
            ("TERM", "xterm-kitty"),
            ("COLORTERM", "truecolor"),
        ]);
        let caps = Capabilities::detect_with_env(&env);
        assert!(caps.is_buffer_terminal);
        assert!(!caps.kitty_keyboard);
        assert!(!caps.osc52);
        assert!(!caps.mouse);
        assert!(!caps.query_ok);
        // Truecolor still honored — Neovim forwards the SGR bytes.
        assert!(caps.truecolor);
        assert_eq!(caps.graphics, GraphicsKind::HalfBlock);
        assert_eq!(caps.host, "neovim:terminal");
    }

    #[test]
    fn vscode_terminal_is_buffer_terminal() {
        let env = snap(&[("TERM_PROGRAM", "vscode"), ("TERM", "xterm-256color")]);
        let caps = Capabilities::detect_with_env(&env);
        assert!(caps.is_buffer_terminal);
        assert!(!caps.osc52);
        assert_eq!(caps.host, "vscode");
    }

    #[test]
    fn kitty_outside_buffer_terminal_lights_up_everything() {
        let env = snap(&[
            ("TERM", "xterm-kitty"),
            ("TERM_PROGRAM", "kitty"),
            ("COLORTERM", "truecolor"),
        ]);
        let caps = Capabilities::detect_with_env(&env);
        assert!(!caps.is_buffer_terminal);
        assert!(caps.truecolor);
        assert!(caps.kitty_keyboard);
        assert!(caps.osc52);
        assert_eq!(caps.graphics, GraphicsKind::Kitty);
        assert!(caps.mouse);
        assert!(caps.focus_events);
        assert!(caps.query_ok);
    }

    #[test]
    fn iterm2_picks_iterm2_graphics_not_kitty() {
        let env = snap(&[("TERM_PROGRAM", "iTerm.app"), ("TERM", "xterm-256color")]);
        let caps = Capabilities::detect_with_env(&env);
        assert_eq!(caps.graphics, GraphicsKind::ITerm2);
        assert!(caps.osc52);
        // Not a kitty terminal — no enhancement flags.
        assert!(!caps.kitty_keyboard);
    }

    #[test]
    fn unknown_terminal_gets_conservative_defaults() {
        let env = snap(&[("TERM", "vt100")]);
        let caps = Capabilities::detect_with_env(&env);
        assert!(!caps.is_buffer_terminal);
        // No COLORTERM and no allowlisted family → assume 256.
        assert!(!caps.truecolor);
        assert!(!caps.kitty_keyboard);
        // Generic terminals don't get OSC52 by default — too many
        // emit the escape into the buffer as visible noise.
        assert!(!caps.osc52);
        assert_eq!(caps.graphics, GraphicsKind::HalfBlock);
        // Mouse still allowed; the viewport / config layer makes
        // the final call.
        assert!(caps.mouse);
        assert!(caps.query_ok);
    }

    #[test]
    fn auto_viewport_picks_inline_for_buffer_terminal() {
        let env = snap(&[("NVIM", "/tmp/nvim.sock")]);
        let caps = Capabilities::detect_with_env(&env);
        assert_eq!(
            caps.auto_viewport(),
            crate::tui::terminal::ViewportMode::Inline,
        );
    }

    #[test]
    fn auto_viewport_picks_fullscreen_for_normal_terminal() {
        let env = snap(&[("TERM_PROGRAM", "iTerm.app")]);
        let caps = Capabilities::detect_with_env(&env);
        assert_eq!(
            caps.auto_viewport(),
            crate::tui::terminal::ViewportMode::Fullscreen,
        );
    }

    #[test]
    fn emacs_term_is_buffer_terminal() {
        let env = snap(&[("INSIDE_EMACS", "29.1,term:0.96")]);
        let caps = Capabilities::detect_with_env(&env);
        assert!(caps.is_buffer_terminal);
        assert_eq!(caps.host, "emacs:term");
    }

    #[test]
    fn resolve_osc52_config_override_wins_in_both_directions() {
        // Force-on inside a buffer-terminal (user knows their host
        // forwards OSC52, e.g. a tmux config we couldn't detect).
        assert!(resolve_osc52(Some("on"), false));
        // Force-off in an OSC52-capable terminal — user prefers the
        // fallback modal so they can read the body before pasting.
        assert!(!resolve_osc52(Some("off"), true));
    }

    #[test]
    fn resolve_osc52_auto_defers_to_caps() {
        // No config or explicit auto → caps value passes through.
        assert!(resolve_osc52(None, true));
        assert!(!resolve_osc52(None, false));
        assert!(resolve_osc52(Some("auto"), true));
        assert!(!resolve_osc52(Some("auto"), false));
    }

    #[test]
    fn resolve_osc52_accepts_synonyms_and_ignores_case() {
        assert!(resolve_osc52(Some("ON"), false));
        assert!(resolve_osc52(Some("True"), false));
        assert!(resolve_osc52(Some(" yes "), false));
        assert!(!resolve_osc52(Some("OFF"), true));
        assert!(!resolve_osc52(Some("false"), true));
        assert!(!resolve_osc52(Some("no"), true));
    }

    #[test]
    fn resolve_osc52_unknown_values_fall_through_to_caps() {
        // Typo'd config: don't erase the user's clipboard path, just
        // fall back to what we'd have picked automatically.
        assert!(resolve_osc52(Some("bogus"), true));
        assert!(!resolve_osc52(Some(""), false));
    }

    #[test]
    fn zellij_passes_through_to_outer_terminal_behavior() {
        // Zellij is a multiplexer, not a buffer-terminal. The outer
        // terminal's capabilities should pass through.
        let env = snap(&[
            ("ZELLIJ", "0.39.2"),
            ("TERM", "xterm-kitty"),
            ("COLORTERM", "truecolor"),
        ]);
        let caps = Capabilities::detect_with_env(&env);
        assert!(!caps.is_buffer_terminal);
        assert!(caps.kitty_keyboard);
        assert!(caps.truecolor);
    }
}
