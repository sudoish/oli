//! `App` — in-memory state of one TUI session.
//!
//! Phase F is intentionally small: a transcript of items, an input
//! buffer, a quit flag. Phase G adds modes (Idle / Thinking /
//! Streaming / AwaitingApproval) and the active-assistant /
//! tool-card bookkeeping; Phase K swaps the input for `tui-textarea`.
//! The shape here is the minimum that makes echo-mode work.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

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
    /// / `:q`). The render loop checks this after every event.
    pub should_quit: bool,
}

#[derive(Debug, Clone)]
pub enum TranscriptItem {
    UserPrompt { body: String },
    System { body: String },
}

impl App {
    pub fn new() -> Self {
        let mut app = Self::default();
        app.transcript.push(TranscriptItem::System {
            body: "oli ready. type a message and press Enter. Ctrl+D to exit.".into(),
        });
        app
    }

    pub fn request_quit(&mut self) {
        self.should_quit = true;
    }

    /// Apply a keypress to the input area / dispatch helpers.
    /// Returns nothing; mutations land on `self`. Intentionally a
    /// flat match so each new key is an obvious one-liner.
    pub fn on_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => self.submit(),
            KeyCode::Esc => {
                // ESC clears the input box without exiting. A
                // double-ESC could exit later; for now ESC is just
                // a "scrub the line" shortcut.
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
                // Ignore Ctrl+<letter> — those are global shortcuts
                // handled in `tui::handle_event`. We let plain
                // Shift through (it's just the uppercase char).
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    return;
                }
                self.input.insert(self.cursor, c);
                self.cursor += c.len_utf8();
            }
            _ => {}
        }
    }

    /// Take the current input, push it as a `UserPrompt` transcript
    /// item, and clear the buffer. Phase G: also kicks off an
    /// agent task. Phase F: pure echo.
    fn submit(&mut self) {
        let trimmed = self.input.trim();
        if trimmed.is_empty() {
            return;
        }
        let body = trimmed.to_string();
        // `:q` is a vim-style escape hatch alongside Ctrl+D for
        // muscle memory. Slash-`/exit` joins the family in Phase G
        // (it routes through the slash registry).
        if body == ":q" {
            self.request_quit();
            return;
        }
        self.transcript
            .push(TranscriptItem::UserPrompt { body: body.clone() });
        // Phase F echoes; Phase G replaces this with the agent
        // call. The `(echo)` marker is a placeholder so Phase F
        // demos visibly.
        self.transcript.push(TranscriptItem::System {
            body: format!("(echo) {}", body),
        });
        self.input.clear();
        self.cursor = 0;
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

    #[test]
    fn typing_appends_to_input_and_moves_cursor() {
        let mut app = App::new();
        for c in "hello".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        assert_eq!(app.input, "hello");
        assert_eq!(app.cursor, 5);
    }

    #[test]
    fn backspace_removes_previous_char() {
        let mut app = App::new();
        for c in "hi".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Backspace));
        assert_eq!(app.input, "h");
        assert_eq!(app.cursor, 1);
    }

    #[test]
    fn left_right_home_end_navigate_cursor() {
        let mut app = App::new();
        for c in "abc".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
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
    fn enter_submits_into_transcript_and_clears_input() {
        let mut app = App::new();
        let starting_items = app.transcript.len();
        for c in "hello".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Enter));
        assert!(app.input.is_empty());
        assert_eq!(app.cursor, 0);
        // user prompt + echo system item.
        assert_eq!(app.transcript.len(), starting_items + 2);
        match &app.transcript[starting_items] {
            TranscriptItem::UserPrompt { body } => assert_eq!(body, "hello"),
            other => panic!("expected UserPrompt, got {:?}", other),
        }
    }

    #[test]
    fn empty_or_whitespace_submission_is_a_noop() {
        let mut app = App::new();
        let starting_items = app.transcript.len();
        for c in "   ".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.transcript.len(), starting_items);
    }

    #[test]
    fn esc_clears_input_without_quitting() {
        let mut app = App::new();
        for c in "draft".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Esc));
        assert!(app.input.is_empty());
        assert!(!app.should_quit);
    }

    #[test]
    fn colon_q_submission_quits() {
        let mut app = App::new();
        for c in ":q".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Enter));
        assert!(app.should_quit);
    }

    #[test]
    fn ctrl_letters_are_ignored_in_the_input_box() {
        // Global Ctrl+C / Ctrl+D are handled at the tui::run
        // level; the App's on_key shouldn't insert their letters.
        let mut app = App::new();
        app.on_key(ctrl('c'));
        app.on_key(ctrl('d'));
        assert!(app.input.is_empty());
    }

    #[test]
    fn unicode_typing_and_backspace_respect_char_boundaries() {
        let mut app = App::new();
        for c in "café".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        assert_eq!(app.input, "café");
        // `é` is 2 bytes in UTF-8; cursor should be 5.
        assert_eq!(app.cursor, 5);
        app.on_key(key(KeyCode::Backspace));
        assert_eq!(app.input, "caf");
        assert_eq!(app.cursor, 3);
    }
}
