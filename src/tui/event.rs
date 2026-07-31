//! Single channel feeding the render loop. Phase G adds the
//! agent-driver-side variants; Phase H adds tool-card events;
//! Phase I adds approval modal events. The variants keep growing
//! as the surface widens — `Clone`/`Debug` derives stay with us
//! by deliberately keeping non-clone state (oneshot senders) out
//! of the variants and in side-channel slots instead.

use crossterm::event::{KeyEvent, MouseEvent};
use serde_json::Value;

#[derive(Debug, Clone)]
pub enum UiEvent {
    /// A user keypress arrived from crossterm. Already filtered to
    /// `KeyEventKind::Press` (Windows fires Press + Release).
    Key(KeyEvent),
    /// Terminal was resized. The render loop redraws on the next
    /// frame; nothing further to do at the App level.
    Resize,
    /// Mouse event (we currently only act on wheel-up/wheel-down
    /// to scroll the transcript; everything else is dropped).
    Mouse(MouseEvent),

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
    /// The driver finished dispatching a slash command. Slash
    /// commands don't go through the turn lifecycle, but the UI
    /// flipped to Thinking on submit — this resets it back to Idle
    /// and clears the cancel sender.
    SlashFinished,
    /// `/exit` (or equivalent) routed through the driver. The UI
    /// quits cleanly.
    Quit,
    /// Driver applied an `Undo` command. `prompt_body` is the
    /// body of the user prompt that was popped (or `None` if
    /// memory had nothing to undo). The TUI uses it to trim the
    /// transcript in lock-step and — when triggered by Ctrl+E —
    /// re-load it into the input box for editing.
    UndoApplied {
        prompt_body: Option<String>,
        load_into_input: bool,
    },

    // ----- Tool-card events (Phase H) -----
    /// Agent dispatched a tool. The render loop pushes a Running
    /// card into the transcript and tracks it by `id` so the
    /// matching `ToolDone` finds the right slot.
    ToolStart {
        id: u64,
        tool: String,
        args_preview: String,
    },
    /// Agent finished a tool. The render loop flips the card to
    /// `Done` with timing + summary + ok flag.
    ToolDone {
        id: u64,
        duration: std::time::Duration,
        summary: String,
        ok: bool,
        /// Phase Y4: full captured tool output, truncated at the
        /// hook boundary. The renderer shows it on demand when the
        /// card is focused + Enter.
        full_output: String,
    },

    /// Phase Y2: provider emitted a chunk of streaming-tool-args JSON
    /// for a not-yet-dispatched tool call (`Edit` / `Write` etc.). The
    /// render loop maintains a streaming-card transcript item per
    /// `provider_tool_id` and updates its preview. When `ToolStart`
    /// later fires for the same call, the streaming card is upgraded
    /// in-place to the running card.
    ToolArgsChunk {
        provider_tool_id: String,
        name: String,
        accumulated_json: String,
    },

    /// Driver picked up fresh `last_usage` + `session_usage`
    /// from the agent. The status bar's token gauge reads from
    /// the last received update.
    UsageUpdate {
        last: Option<crate::providers::Usage>,
        session: crate::providers::Usage,
    },

    // ----- Approval modal events (Phase I) -----
    /// Policy gate returned `Decision::Ask`; the agent task is
    /// suspended on the approver's oneshot. The render loop pops
    /// a modal and waits for `y/n/a/d/ESC`. The matching response
    /// sender is stashed in `tui::approver::PendingApproval`,
    /// keyed implicitly (single-slot — only one approval pending
    /// at a time).
    ApprovalRequested {
        tool: String,
        args: Value,
        reason: String,
    },

    // ----- Wizard async events (Ollama onboarding) -----
    /// Background Ollama probe finished — render loop updates
    /// `WizardState::daemon`.
    WizardOllamaProbed(crate::wizard_init::OllamaProbe),
    /// One chunk from a streaming model pull. Updates
    /// `WizardState::pull` so the progress bar advances.
    WizardOllamaPullEvent(crate::wizard_init::PullEvent),
}

/// User's response to an approval modal. The `Always*` variants
/// tell the `TuiApprover` to remember the (tool, args) fingerprint
/// so subsequent identical requests auto-resolve. `PersistAllow`
/// goes one step further: writes the fingerprint to
/// `~/.config/oli/policy-allow.json` so it survives across runs.
#[derive(Debug, Clone, Copy)]
pub enum ApprovalResponse {
    Yes,
    No,
    AlwaysAllow,
    AlwaysDeny,
    /// Caps `[A]` — like `AlwaysAllow` but also writes through
    /// to disk. Picked up by future runs via
    /// `PersistedAllowList::open()`.
    PersistAllow,
}

/// Map the inline approval list cursor to a response. Order must
/// match `crate::tui::app::APPROVAL_OPTIONS`.
pub fn approval_response_for(index: usize) -> ApprovalResponse {
    match index {
        0 => ApprovalResponse::Yes,
        1 => ApprovalResponse::No,
        2 => ApprovalResponse::AlwaysAllow,
        3 => ApprovalResponse::PersistAllow,
        _ => ApprovalResponse::AlwaysDeny,
    }
}


