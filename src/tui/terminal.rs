//! Terminal lifecycle. Enable raw mode + alternate screen + mouse
//! capture on construction; restore everything on Drop so a panic
//! mid-render leaves the user with a usable terminal.

use std::io::{Stdout, stdout};

use crossterm::ExecutableCommand;
use crossterm::cursor::Show;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

pub struct TerminalGuard {
    terminal: Tui,
}

impl TerminalGuard {
    /// Set up the terminal for TUI rendering and return a guard.
    /// The `Drop` impl tears everything back down — never call
    /// `restore` directly.
    pub fn enter() -> std::io::Result<Self> {
        enable_raw_mode()?;
        let mut out = stdout();
        out.execute(EnterAlternateScreen)?;
        out.execute(EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout());
        let terminal = Terminal::new(backend)?;
        Ok(Self { terminal })
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
        let _ = out.execute(DisableMouseCapture);
        let _ = out.execute(LeaveAlternateScreen);
        let _ = out.execute(Show);
    }
}
