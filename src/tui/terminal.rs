//! Terminal lifecycle. Enable raw mode + the chosen viewport mode
//! on construction; restore everything on Drop so a panic mid-render
//! leaves the user with a usable terminal.
//!
//! Two viewport modes:
//!
//! - `Fullscreen` — alternate screen, mouse capture on. The pre-W1
//!   default for a fresh terminal (iTerm2, kitty, etc.).
//! - `Inline` — no alt-screen, no mouse capture by default; ratatui
//!   reserves a fixed block of rows below the cursor in the host
//!   buffer and the transcript stays as scrollback when oli exits.
//!   Designed for buffer-terminals (`:terminal` inside Neovim,
//!   VSCode integrated terminal, Helix `:sh`, Zellij, Emacs term).

use std::io::{Stdout, stdout};

use crossterm::ExecutableCommand;
use crossterm::cursor::Show;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::{TerminalOptions, Viewport};

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

/// Default inline-mode height, in rows. Picked to fit a single
/// status row + a small transcript window + the input box without
/// dominating the host buffer. Phase W1 keeps this constant; a
/// follow-up may expose it via `[ui].inline_height`.
const DEFAULT_INLINE_HEIGHT: u16 = 32;
/// Floor: ratatui's three-pane layout (status + transcript + input)
/// can't render meaningfully below this; we clamp up rather than
/// crash on tiny terminals.
const MIN_INLINE_HEIGHT: u16 = 12;
/// Slack rows we leave at the top of the host buffer so the user
/// can still see the line they ran `oli` from.
const INLINE_HEADROOM: u16 = 2;

/// What kind of viewport the TUI renders into. Resolved once at
/// startup from the CLI flag + config + (Phase W2) capabilities.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ViewportMode {
    /// Owns the whole terminal via alternate-screen.
    #[default]
    Fullscreen,
    /// Renders inline in the host buffer.
    Inline,
}

/// User intent — the unresolved choice from CLI flag or config.
/// `Auto` resolves to a concrete `ViewportMode` via `resolve_mode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ViewportChoice {
    #[default]
    Auto,
    Fullscreen,
    Inline,
}

impl ViewportChoice {
    /// Parse a config string. Unknown values fall back to `Auto`
    /// rather than erroring — the TUI still launches if a user
    /// typo'd their config.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "fullscreen" | "full" => Self::Fullscreen,
            "inline" => Self::Inline,
            _ => Self::Auto,
        }
    }
}

/// Resolve the final viewport from (in priority order):
/// 1. an explicit CLI flag (`Some(Fullscreen)` / `Some(Inline)`),
/// 2. the config value (`ViewportChoice`),
/// 3. the auto-detection result, supplied by the caller (Phase W2
///    will compute it from `Capabilities`; for now W1 passes the
///    fallback default — `Fullscreen` — so `Auto` keeps the
///    pre-W1 behavior).
pub fn resolve_mode(
    flag: Option<ViewportMode>,
    config: ViewportChoice,
    auto_default: ViewportMode,
) -> ViewportMode {
    if let Some(m) = flag {
        return m;
    }
    match config {
        ViewportChoice::Fullscreen => ViewportMode::Fullscreen,
        ViewportChoice::Inline => ViewportMode::Inline,
        ViewportChoice::Auto => auto_default,
    }
}

/// Compute the inline viewport height for the current terminal
/// rows. Bounded by `MIN_INLINE_HEIGHT` and
/// `terminal_rows - INLINE_HEADROOM` so we never ask ratatui to
/// reserve more space than the host has.
pub fn inline_height(terminal_rows: u16) -> u16 {
    let cap = terminal_rows.saturating_sub(INLINE_HEADROOM);
    DEFAULT_INLINE_HEIGHT.min(cap).max(MIN_INLINE_HEIGHT)
}

/// Decide whether to enable mouse capture, in priority order:
///
/// 1. Explicit config override (`[ui].mouse = true | false`).
/// 2. `caps.mouse == false` (host doesn't pass mouse cleanly —
///    Neovim `:terminal`, VSCode integrated terminal) ⇒ off.
/// 3. Inline viewport ⇒ off (the host buffer expects to own the
///    scroll-wheel; W7 documents the split).
/// 4. Fullscreen + capable host ⇒ on (pre-W3 default).
pub fn resolve_mouse(
    config: Option<bool>,
    caps_mouse_allowed: bool,
    viewport: ViewportMode,
) -> bool {
    if let Some(b) = config {
        return b;
    }
    if !caps_mouse_allowed {
        return false;
    }
    matches!(viewport, ViewportMode::Fullscreen)
}

pub struct TerminalGuard {
    terminal: Tui,
    mode: ViewportMode,
    mouse_captured: bool,
}

impl TerminalGuard {
    /// Set up the terminal in the requested viewport mode and
    /// return a guard. The `Drop` impl tears everything back
    /// down — never call `restore` directly.
    ///
    /// `mouse_capture` is honored only in fullscreen mode for now;
    /// Phase W3 will widen the contract so inline mode can opt in.
    pub fn enter(mode: ViewportMode, mouse_capture: bool) -> std::io::Result<Self> {
        enable_raw_mode()?;
        let mut out = stdout();
        match mode {
            ViewportMode::Fullscreen => {
                out.execute(EnterAlternateScreen)?;
            }
            ViewportMode::Inline => {
                // No alt-screen: ratatui's Viewport::Inline reserves
                // rows below the cursor in the existing buffer.
            }
        }
        let want_mouse = mouse_capture && matches!(mode, ViewportMode::Fullscreen);
        if want_mouse {
            out.execute(EnableMouseCapture)?;
        }
        let backend = CrosstermBackend::new(stdout());
        let terminal = match mode {
            ViewportMode::Fullscreen => Terminal::new(backend)?,
            ViewportMode::Inline => {
                let rows = crossterm::terminal::size().map(|(_, r)| r).unwrap_or(40);
                Terminal::with_options(
                    backend,
                    TerminalOptions {
                        viewport: Viewport::Inline(inline_height(rows)),
                    },
                )?
            }
        };
        Ok(Self {
            terminal,
            mode,
            mouse_captured: want_mouse,
        })
    }

    pub fn terminal_mut(&mut self) -> &mut Tui {
        &mut self.terminal
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // Best-effort restore. Each step is independent so a
        // failure in one doesn't skip the rest. If we panic on
        // drop the terminal stays broken — but the regular case
        // (graceful exit) and panic-during-draw both get cleaned
        // up.
        let _ = disable_raw_mode();
        let mut out = stdout();
        if self.mouse_captured {
            let _ = out.execute(DisableMouseCapture);
        }
        if matches!(self.mode, ViewportMode::Fullscreen) {
            let _ = out.execute(LeaveAlternateScreen);
        } else {
            // In inline mode we want the cursor to land on the
            // line after the rendered block so the next shell
            // prompt doesn't paint over it.
            let _ = self.terminal.insert_before(0, |_| {});
        }
        let _ = out.execute(Show);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_choice_accepts_named_values() {
        assert_eq!(ViewportChoice::parse("auto"), ViewportChoice::Auto);
        assert_eq!(ViewportChoice::parse("Auto"), ViewportChoice::Auto);
        assert_eq!(
            ViewportChoice::parse("fullscreen"),
            ViewportChoice::Fullscreen
        );
        assert_eq!(ViewportChoice::parse("FULL"), ViewportChoice::Fullscreen);
        assert_eq!(ViewportChoice::parse("inline"), ViewportChoice::Inline);
        assert_eq!(ViewportChoice::parse("  inline  "), ViewportChoice::Inline);
    }

    #[test]
    fn parse_choice_falls_back_to_auto_on_garbage() {
        // Typo'd config shouldn't crash the TUI — fall through to
        // auto rather than a panic or hard error.
        assert_eq!(ViewportChoice::parse(""), ViewportChoice::Auto);
        assert_eq!(ViewportChoice::parse("bogus"), ViewportChoice::Auto);
    }

    #[test]
    fn flag_overrides_config_and_auto() {
        // --inline beats `viewport = "fullscreen"` beats whatever
        // auto-detection wants.
        assert_eq!(
            resolve_mode(
                Some(ViewportMode::Inline),
                ViewportChoice::Fullscreen,
                ViewportMode::Fullscreen,
            ),
            ViewportMode::Inline,
        );
        assert_eq!(
            resolve_mode(
                Some(ViewportMode::Fullscreen),
                ViewportChoice::Inline,
                ViewportMode::Inline,
            ),
            ViewportMode::Fullscreen,
        );
    }

    #[test]
    fn config_overrides_auto_default() {
        assert_eq!(
            resolve_mode(None, ViewportChoice::Inline, ViewportMode::Fullscreen),
            ViewportMode::Inline,
        );
        assert_eq!(
            resolve_mode(None, ViewportChoice::Fullscreen, ViewportMode::Inline),
            ViewportMode::Fullscreen,
        );
    }

    #[test]
    fn auto_uses_auto_default_when_unset() {
        assert_eq!(
            resolve_mode(None, ViewportChoice::Auto, ViewportMode::Fullscreen),
            ViewportMode::Fullscreen,
        );
        assert_eq!(
            resolve_mode(None, ViewportChoice::Auto, ViewportMode::Inline),
            ViewportMode::Inline,
        );
    }

    #[test]
    fn inline_height_clamps_to_floor_on_tiny_terminal() {
        // A 10-row host has no room for the 32-row default; we
        // clamp up to MIN_INLINE_HEIGHT rather than down to a
        // unusable 8 rows.
        assert_eq!(inline_height(10), MIN_INLINE_HEIGHT);
        assert_eq!(inline_height(0), MIN_INLINE_HEIGHT);
    }

    #[test]
    fn inline_height_respects_headroom_on_medium_terminal() {
        // 20 rows tall: cap at 20 - INLINE_HEADROOM = 18.
        assert_eq!(inline_height(20), 18);
    }

    #[test]
    fn inline_height_caps_at_default_on_tall_terminal() {
        assert_eq!(inline_height(80), DEFAULT_INLINE_HEIGHT);
        assert_eq!(inline_height(200), DEFAULT_INLINE_HEIGHT);
    }

    #[test]
    fn mouse_config_override_wins_in_both_directions() {
        // Force-on inside a buffer-terminal (user knows their host
        // forwards mouse) — capture turns on even though caps says
        // mouse=false.
        assert!(resolve_mouse(Some(true), false, ViewportMode::Inline));
        // Force-off in fullscreen kitty — capture turns off even
        // though caps and viewport would both default it on.
        assert!(!resolve_mouse(Some(false), true, ViewportMode::Fullscreen));
    }

    #[test]
    fn mouse_off_in_buffer_terminal_by_default() {
        // No override; caps says mouse is unsafe (Neovim :terminal) —
        // capture stays off regardless of viewport.
        assert!(!resolve_mouse(None, false, ViewportMode::Fullscreen));
        assert!(!resolve_mouse(None, false, ViewportMode::Inline));
    }

    #[test]
    fn mouse_off_in_inline_mode_by_default() {
        // Inline mode wants to share the scroll-wheel with the host
        // buffer — default off even when caps would allow it.
        assert!(!resolve_mouse(None, true, ViewportMode::Inline));
    }

    #[test]
    fn mouse_on_in_fullscreen_capable_host_by_default() {
        // The pre-W3 default for a fresh terminal.
        assert!(resolve_mouse(None, true, ViewportMode::Fullscreen));
    }
}
