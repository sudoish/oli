//! Single channel feeding the render loop.
//!
//! Phase F has just two producers: the input task (crossterm event
//! stream) and the implicit "user requested redraw" path. Phase G
//! adds the agent task (streaming chunks) and the hook bridge
//! (PreToolUse / PostToolUse / Stop). The variants land in this
//! enum as we grow.

use crossterm::event::KeyEvent;

#[derive(Debug, Clone)]
pub enum UiEvent {
    /// A user keypress arrived from crossterm. Already filtered to
    /// `KeyEventKind::Press` (Windows fires Press + Release).
    Key(KeyEvent),
    /// Terminal was resized. The render loop redraws on the next
    /// frame; nothing further to do at the App level.
    Resize,
}
