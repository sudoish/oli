//! `App` — in-memory state of one TUI session.
//!
//! Phase F was echo-only. Phase G introduces real modes (Idle /
//! Thinking / Streaming) tracked alongside the active assistant
//! item, plus a cancel-sender slot the UI hands to the driver per
//! command.

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::oneshot;

#[derive(Default)]
pub struct App {
    /// Items rendered top-to-bottom in the transcript pane.
    pub transcript: Vec<TranscriptItem>,
    /// Single-line input buffer. Phase K replaces with multi-line
    /// tui-textarea; the App-level interface stays a string-in
    /// shape.
    pub input: String,
    /// Cursor position within `input` (byte index, char-boundary
    /// safe). Lets Left/Right work without a trip through
    /// rustyline.
    pub cursor: usize,
    /// Set true when the user has asked to leave (Ctrl+C / Ctrl+D
    /// / `:q` / driver-side `Quit`). The render loop checks this
    /// after every event.
    pub should_quit: bool,
    /// What's the loop doing right now. Drives the "ready / thinking
    /// / streaming" indicator and gates new submissions.
    pub mode: Mode,
    /// Index in `transcript` of the assistant message we're
    /// appending streamed chunks to. None when we're not in a
    /// streaming turn.
    pub active_assistant: Option<usize>,
    /// Cancel sender for the in-flight driver command. UI uses it
    /// to interrupt on Ctrl+C while busy. None when Idle.
    pub cancel_tx: Option<oneshot::Sender<()>>,
}

pub enum Mode {
    Idle,
    Thinking { since: Instant },
    Streaming,
}

impl Default for Mode {
    fn default() -> Self {
        Mode::Idle
    }
}

#[derive(Debug, Clone)]
pub enum TranscriptItem {
    UserPrompt {
        body: String,
    },
    /// An assistant message. `done` flips true once the turn
    /// finishes so the UI can stop rendering the streaming
    /// indicator next to it.
    Assistant {
        body: String,
        done: bool,
    },
    /// System / harness notice (slash output, cancel marker,
    /// errors). Not part of the model's transcript.
    System {
        body: String,
    },
}

/// Thing the App wants the outside loop to do for it. Returning
/// these from `submit()` keeps the App pure — no `tokio::spawn`,
/// no channels — and the driver-spawn glue lives in `tui::run`.
#[derive(Debug)]
pub enum SubmitAction {
    /// User submitted a prompt; spawn an agent run.
    Prompt(String),
    /// User submitted a slash; dispatch through the slash registry.
    Slash(String),
    /// Submission was effectively a no-op (whitespace, `:q` already
    /// handled, etc).
    None,
}

impl App {
    pub fn new() -> Self {
        let mut app = Self::default();
        app.transcript.push(TranscriptItem::System {
            body: "oli ready. type a message and press Enter. /help for commands, Ctrl+D to exit."
                .into(),
        });
        app
    }

    pub fn request_quit(&mut self) {
        self.should_quit = true;
    }

    pub fn is_busy(&self) -> bool {
        !matches!(self.mode, Mode::Idle)
    }

    /// Reaction to a keypress. Keystrokes that should turn into
    /// driver commands (Enter on a non-empty line) come back via
    /// `SubmitAction`; the caller fires the channel.
    pub fn on_key(&mut self, key: KeyEvent) -> SubmitAction {
        match key.code {
            KeyCode::Enter => return self.submit(),
            KeyCode::Esc => {
                // ESC clears the input box without exiting. While
                // busy, ESC does nothing — the caller handles
                // cancel via Ctrl+C.
                self.input.clear();
                self.cursor = 0;
            }
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    let prev = prev_char_boundary(&self.input, self.cursor);
                    self.input.replace_range(prev..self.cursor, "");
                    self.cursor = prev;
                }
            }
            KeyCode::Delete => {
                if self.cursor < self.input.len() {
                    let next = next_char_boundary(&self.input, self.cursor);
                    self.input.replace_range(self.cursor..next, "");
                }
            }
            KeyCode::Left => {
                if self.cursor > 0 {
                    self.cursor = prev_char_boundary(&self.input, self.cursor);
                }
            }
            KeyCode::Right => {
                if self.cursor < self.input.len() {
                    self.cursor = next_char_boundary(&self.input, self.cursor);
                }
            }
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.input.len(),
            KeyCode::Char(c) => {
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    return SubmitAction::None;
                }
                self.input.insert(self.cursor, c);
                self.cursor += c.len_utf8();
            }
            _ => {}
        }
        SubmitAction::None
    }

    fn submit(&mut self) -> SubmitAction {
        // Don't accept new submissions while a turn is in flight.
        // The render layer hides the input cursor in busy modes
        // anyway; this is a belt-and-suspenders gate.
        if self.is_busy() {
            return SubmitAction::None;
        }
        let trimmed = self.input.trim();
        if trimmed.is_empty() {
            return SubmitAction::None;
        }
        let body = trimmed.to_string();
        self.input.clear();
        self.cursor = 0;

        // `:q` is a vim-style escape hatch alongside Ctrl+D for
        // muscle memory. `/exit` routes through the slash
        // registry and lands as a SubmitAction::Slash.
        if body == ":q" {
            self.request_quit();
            return SubmitAction::None;
        }
        if let Some(rest) = body.strip_prefix('/') {
            // Don't push a transcript item for slash commands —
            // their output is what the user wants to see, not the
            // command itself echoed back.
            return SubmitAction::Slash(rest.to_string());
        }
        self.transcript
            .push(TranscriptItem::UserPrompt { body: body.clone() });
        SubmitAction::Prompt(body)
    }

    // ----- Driver-side event handlers -----

    pub fn on_turn_started(&mut self) {
        self.mode = Mode::Thinking {
            since: Instant::now(),
        };
        // Pre-create the assistant transcript item so the user
        // sees a slot for the response right away.
        self.transcript.push(TranscriptItem::Assistant {
            body: String::new(),
            done: false,
        });
        self.active_assistant = Some(self.transcript.len() - 1);
    }

    pub fn on_content_chunk(&mut self, chunk: &str) {
        if matches!(self.mode, Mode::Thinking { .. }) {
            self.mode = Mode::Streaming;
        }
        if let Some(idx) = self.active_assistant {
            if let Some(TranscriptItem::Assistant { body, .. }) = self.transcript.get_mut(idx) {
                body.push_str(chunk);
            }
        }
    }

    pub fn on_turn_finished(&mut self, _final_content: &str) {
        // The chunks already populated the assistant body; we
        // ignore `final_content` here. (We keep the parameter so
        // future agents that emit a non-streaming summary at the
        // end can replace the body if they want.)
        if let Some(idx) = self.active_assistant.take() {
            if let Some(TranscriptItem::Assistant { done, .. }) = self.transcript.get_mut(idx) {
                *done = true;
            }
        }
        self.mode = Mode::Idle;
        self.cancel_tx = None;
    }

    pub fn on_turn_error(&mut self, msg: &str) {
        if let Some(idx) = self.active_assistant.take() {
            if let Some(TranscriptItem::Assistant { done, body, .. }) =
                self.transcript.get_mut(idx)
            {
                if body.is_empty() {
                    *body = format!("(error: {})", msg);
                }
                *done = true;
            }
        }
        self.transcript.push(TranscriptItem::System {
            body: format!("error: {}", msg),
        });
        self.mode = Mode::Idle;
        self.cancel_tx = None;
    }

    pub fn on_turn_cancelled(&mut self) {
        if let Some(idx) = self.active_assistant.take() {
            if let Some(TranscriptItem::Assistant { done, body, .. }) =
                self.transcript.get_mut(idx)
            {
                if body.is_empty() {
                    *body = "(cancelled before any output)".into();
                }
                *done = true;
            }
        }
        self.transcript.push(TranscriptItem::System {
            body: "(cancelled)".into(),
        });
        self.mode = Mode::Idle;
        self.cancel_tx = None;
    }

    pub fn on_system_note(&mut self, body: String) {
        self.transcript.push(TranscriptItem::System { body });
    }

    /// Take the cancel sender out of the App so the caller can
    /// signal cancel. Subsequent calls return None. Used by Ctrl+C
    /// while busy.
    pub fn take_cancel_sender(&mut self) -> Option<oneshot::Sender<()>> {
        self.cancel_tx.take()
    }

    pub fn set_cancel_sender(&mut self, tx: oneshot::Sender<()>) {
        self.cancel_tx = Some(tx);
    }
}

/// Step `cursor` to the previous char boundary in `s`. Assumes
/// `cursor` is already a valid boundary; otherwise we'd have to
/// scan more carefully.
fn prev_char_boundary(s: &str, cursor: usize) -> usize {
    let mut i = cursor.saturating_sub(1);
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn next_char_boundary(s: &str, cursor: usize) -> usize {
    let mut i = (cursor + 1).min(s.len());
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn type_str(app: &mut App, s: &str) {
        for c in s.chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn typing_appends_to_input_and_moves_cursor() {
        let mut app = App::new();
        type_str(&mut app, "hello");
        assert_eq!(app.input, "hello");
        assert_eq!(app.cursor, 5);
    }

    #[test]
    fn backspace_removes_previous_char() {
        let mut app = App::new();
        type_str(&mut app, "hi");
        app.on_key(key(KeyCode::Backspace));
        assert_eq!(app.input, "h");
        assert_eq!(app.cursor, 1);
    }

    #[test]
    fn left_right_home_end_navigate_cursor() {
        let mut app = App::new();
        type_str(&mut app, "abc");
        app.on_key(key(KeyCode::Home));
        assert_eq!(app.cursor, 0);
        app.on_key(key(KeyCode::Right));
        assert_eq!(app.cursor, 1);
        app.on_key(key(KeyCode::End));
        assert_eq!(app.cursor, 3);
        app.on_key(key(KeyCode::Left));
        assert_eq!(app.cursor, 2);
    }

    #[test]
    fn enter_on_plain_text_returns_prompt_action_and_pushes_user_item() {
        let mut app = App::new();
        let starting_items = app.transcript.len();
        type_str(&mut app, "hello");
        let action = app.on_key(key(KeyCode::Enter));
        assert!(matches!(action, SubmitAction::Prompt(ref body) if body == "hello"));
        assert!(app.input.is_empty());
        assert_eq!(app.transcript.len(), starting_items + 1);
        match &app.transcript[starting_items] {
            TranscriptItem::UserPrompt { body } => assert_eq!(body, "hello"),
            other => panic!("expected UserPrompt, got {:?}", other),
        }
    }

    #[test]
    fn enter_on_slash_returns_slash_action_without_pushing_user_item() {
        let mut app = App::new();
        let starting_items = app.transcript.len();
        type_str(&mut app, "/help");
        let action = app.on_key(key(KeyCode::Enter));
        assert!(matches!(action, SubmitAction::Slash(ref body) if body == "help"));
        // Slash invocations don't get echoed as user prompts; the
        // command's output (a SystemNote) is what the user sees.
        assert_eq!(app.transcript.len(), starting_items);
    }

    #[test]
    fn empty_or_whitespace_submission_is_a_noop() {
        let mut app = App::new();
        let starting_items = app.transcript.len();
        type_str(&mut app, "   ");
        let action = app.on_key(key(KeyCode::Enter));
        assert!(matches!(action, SubmitAction::None));
        assert_eq!(app.transcript.len(), starting_items);
    }

    #[test]
    fn esc_clears_input_without_quitting() {
        let mut app = App::new();
        type_str(&mut app, "draft");
        app.on_key(key(KeyCode::Esc));
        assert!(app.input.is_empty());
        assert!(!app.should_quit);
    }

    #[test]
    fn colon_q_submission_quits() {
        let mut app = App::new();
        type_str(&mut app, ":q");
        app.on_key(key(KeyCode::Enter));
        assert!(app.should_quit);
    }

    #[test]
    fn ctrl_letters_are_ignored_in_the_input_box() {
        let mut app = App::new();
        app.on_key(ctrl('c'));
        app.on_key(ctrl('d'));
        assert!(app.input.is_empty());
    }

    #[test]
    fn unicode_typing_and_backspace_respect_char_boundaries() {
        let mut app = App::new();
        type_str(&mut app, "café");
        assert_eq!(app.input, "café");
        assert_eq!(app.cursor, 5);
        app.on_key(key(KeyCode::Backspace));
        assert_eq!(app.input, "caf");
        assert_eq!(app.cursor, 3);
    }

    #[test]
    fn streaming_lifecycle_appends_chunks_to_active_assistant_item() {
        let mut app = App::new();
        let prior = app.transcript.len();
        app.on_turn_started();
        // Created an empty Assistant slot; mode flipped to Thinking.
        assert_eq!(app.transcript.len(), prior + 1);
        assert!(matches!(app.mode, Mode::Thinking { .. }));
        assert!(app.active_assistant.is_some());

        app.on_content_chunk("hello");
        assert!(matches!(app.mode, Mode::Streaming));
        match &app.transcript[prior] {
            TranscriptItem::Assistant { body, done } => {
                assert_eq!(body, "hello");
                assert!(!*done);
            }
            _ => panic!("expected Assistant item"),
        }
        app.on_content_chunk(" world");
        match &app.transcript[prior] {
            TranscriptItem::Assistant { body, .. } => assert_eq!(body, "hello world"),
            _ => panic!(),
        }

        app.on_turn_finished("hello world");
        match &app.transcript[prior] {
            TranscriptItem::Assistant { done, .. } => assert!(*done),
            _ => panic!(),
        }
        assert!(matches!(app.mode, Mode::Idle));
        assert!(app.active_assistant.is_none());
    }

    #[test]
    fn turn_cancelled_marks_assistant_done_and_pushes_marker() {
        let mut app = App::new();
        app.on_turn_started();
        app.on_content_chunk("partial...");
        app.on_turn_cancelled();
        assert!(matches!(app.mode, Mode::Idle));
        // Last item is the cancellation marker.
        match app.transcript.last().unwrap() {
            TranscriptItem::System { body } => assert!(body.contains("cancelled")),
            _ => panic!(),
        }
    }

    #[test]
    fn turn_error_records_message_and_returns_to_idle() {
        let mut app = App::new();
        app.on_turn_started();
        app.on_turn_error("provider exploded");
        assert!(matches!(app.mode, Mode::Idle));
        match app.transcript.last().unwrap() {
            TranscriptItem::System { body } => {
                assert!(body.contains("provider exploded"));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn submitting_while_busy_returns_none_and_keeps_input() {
        let mut app = App::new();
        app.on_turn_started(); // simulate "thinking"
        type_str(&mut app, "hello");
        let action = app.on_key(key(KeyCode::Enter));
        assert!(matches!(action, SubmitAction::None));
        // Input was not cleared because submission was rejected.
        assert_eq!(app.input, "hello");
    }
}
