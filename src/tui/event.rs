//! Single channel feeding the render loop. Phase G adds the
//! agent-driver-side variants; Phase H adds tool-card events;
//! later phases keep growing it as the surface widens.

use crossterm::event::KeyEvent;

#[derive(Debug, Clone)]
pub enum UiEvent {
    /// A user keypress arrived from crossterm. Already filtered to
    /// `KeyEventKind::Press` (Windows fires Press + Release).
    Key(KeyEvent),
    /// Terminal was resized. The render loop redraws on the next
    /// frame; nothing further to do at the App level.
    Resize,

    // ----- Driver-side events (Phase G) -----
    /// The agent task has started a new turn. The UI flips to
    /// Thinking mode and creates a fresh active-assistant
    /// transcript item ready to receive chunks.
    TurnStarted,
    /// A content delta arrived from the provider's stream. App
    /// appends it to the active-assistant item; the first chunk
    /// flips Thinking → Streaming.
    ContentChunk(String),
    /// The agent produced a final response with no further tool
    /// calls. App marks the active item done and returns to Idle.
    TurnFinished { final_content: String },
    /// The agent run errored out (provider fault, etc). App
    /// surfaces the message and returns to Idle.
    TurnError(String),
    /// The user cancelled the in-flight turn. Memory has already
    /// been truncated by the driver; the UI just resets mode.
    TurnCancelled,
    /// A slash command produced human-readable text for the
    /// transcript (slash output, errors, hints).
    SystemNote(String),
    /// `/exit` (or equivalent) routed through the driver. The UI
    /// quits cleanly.
    Quit,
}
