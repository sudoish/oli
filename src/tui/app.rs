//! `App` — in-memory state of one TUI session.
//!
//! Phase K replaced the hand-rolled single-line buffer with
//! `tui_textarea::TextArea`, added Up/Down + Ctrl-R history
//! navigation, and a completion-menu slot for slash and `@path`
//! popups.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::Value;
use tokio::sync::oneshot;
use tui_textarea::{CursorMove, Input, Key as TaKey, TextArea};

use crate::tui::completion::{self, CompletionContext};
use crate::tui::markdown::Theme;

/// What kind of in-flight completion is being offered.
#[derive(Debug, Clone)]
pub enum CompletionKind {
    /// `/<query>` at the start of the input. Candidates are slash
    /// command names. Replacement target is the whole `/<query>`.
    Slash,
    /// `@<query>` at a word boundary. Candidates are entries from
    /// `base_dir` whose name starts with the tail of the query.
    Path { base_dir: PathBuf },
}

#[derive(Debug, Clone)]
pub struct CompletionMenu {
    pub kind: CompletionKind,
    pub candidates: Vec<String>,
    pub selected: usize,
    /// What we'll replace in the buffer when the user accepts.
    /// `query` is the substring under the cursor (e.g. `cos` for
    /// `/cos<TAB>`); `replace_start_byte` is the byte offset on
    /// the active line where the trigger char (`/` or `@`) lives.
    pub query: String,
    pub replace_start_byte: usize,
}

#[derive(Debug, Clone)]
pub struct ApprovalState {
    pub id: u64,
    pub tool: String,
    pub reason: String,
    pub preview: String,
    pub scroll: u16,
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
    Assistant {
        body: String,
        done: bool,
    },
    System {
        body: String,
    },
    ToolCard {
        id: u64,
        tool: String,
        args_preview: String,
        state: ToolCardState,
    },
}

#[derive(Debug, Clone)]
pub enum ToolCardState {
    Running { started_at: Instant },
    Done {
        duration: Duration,
        summary: String,
        ok: bool,
    },
}

pub struct App {
    pub transcript: Vec<TranscriptItem>,
    pub input: TextArea<'static>,
    pub should_quit: bool,
    pub mode: Mode,
    pub active_assistant: Option<usize>,
    pub cancel_tx: Option<oneshot::Sender<()>>,
    pub active_tools: HashMap<u64, usize>,
    pub approval: Option<ApprovalState>,

    /// Slash command names, sorted, mirroring the live registry
    /// in the driver. Updated at startup and on `/plugins reload`
    /// via `set_slash_names`.
    pub slash_names: Vec<String>,

    /// Open completion popup, if any. `None` when the user isn't
    /// currently completing.
    pub completion: Option<CompletionMenu>,

    /// History of submitted prompts, oldest first. Persisted to
    /// `~/.config/oli/tui-history.jsonl` between sessions.
    pub history: Vec<String>,
    /// Current position when navigating with Up/Down. `None` when
    /// the user is composing a fresh draft.
    pub history_cursor: Option<usize>,
    /// Saved-on-first-Up draft so Down past the end can restore
    /// what the user was typing.
    pub history_draft: Option<String>,

    /// Transcript scroll position. `None` means "stick to bottom"
    /// (the default). The render layer pins to the latest line on
    /// each frame. PgUp / Home / mouse-wheel-up detach into
    /// `Some(offset)`. PgDn / End / mouse-wheel-down re-attach
    /// once we reach the bottom again.
    pub scroll_manual: Option<u16>,
    /// Lines that arrived while we were detached (`scroll_manual`
    /// is `Some`). Reset on re-attach. The status bar / footer
    /// shows this so a user scrolled away knows new content is
    /// queued behind them.
    pub unread_lines: u16,
    /// Cached on each render so key handlers know the bottom-
    /// limit (max valid offset) without recomputing the
    /// transcript layout. Updated by `ui::draw`.
    pub scroll_max: u16,
    /// Cached transcript-pane height for PgUp/PgDn step size.
    pub scroll_viewport_height: u16,

    /// Color theme for markdown / syntax-highlighted content.
    /// Detected from `$COLORFGBG` at TUI startup; defaults to
    /// dark on detection failure.
    pub theme: Theme,

    /// Status-bar fields. Identity (model / session / branch /
    /// ctx_window) is set once at startup; usage is updated by
    /// the driver after each chat round.
    pub status: StatusModel,
}

/// Aggregate of every field the status bar can display. Optional
/// fields render as "—" or get dropped on narrow terminals.
#[derive(Clone, Debug, Default)]
pub struct StatusModel {
    pub session_id: Option<String>,
    pub model: String,
    pub ctx_window: u32,
    pub branch: Option<String>,
    pub last_usage: Option<crate::providers::Usage>,
    pub session_usage: crate::providers::Usage,
}

impl Default for App {
    fn default() -> Self {
        Self {
            transcript: Vec::new(),
            input: build_textarea(),
            should_quit: false,
            mode: Mode::Idle,
            active_assistant: None,
            cancel_tx: None,
            active_tools: HashMap::new(),
            approval: None,
            slash_names: Vec::new(),
            completion: None,
            history: Vec::new(),
            history_cursor: None,
            history_draft: None,
            scroll_manual: None,
            unread_lines: 0,
            scroll_max: 0,
            scroll_viewport_height: 0,
            theme: Theme::Dark,
            status: StatusModel::default(),
        }
    }
}

#[derive(Debug)]
pub enum SubmitAction {
    Prompt(String),
    Slash(String),
    None,
}

impl App {
    pub fn new() -> Self {
        let mut app = Self::default();
        app.theme = Theme::detect();
        app.transcript.push(TranscriptItem::System {
            body: "oli ready. type a message and press Enter (Shift+Enter for newline). \
                   /help for commands, Ctrl+D to exit."
                .into(),
        });
        app
    }

    pub fn set_slash_names(&mut self, mut names: Vec<String>) {
        names.sort();
        self.slash_names = names;
    }

    pub fn set_status(&mut self, status: StatusModel) {
        self.status = status;
    }

    pub fn update_usage(
        &mut self,
        last: Option<crate::providers::Usage>,
        session: crate::providers::Usage,
    ) {
        self.status.last_usage = last;
        self.status.session_usage = session;
    }

    pub fn set_history(&mut self, history: Vec<String>) {
        self.history = history;
        self.history_cursor = None;
        self.history_draft = None;
    }

    pub fn request_quit(&mut self) {
        self.should_quit = true;
    }

    pub fn is_busy(&self) -> bool {
        !matches!(self.mode, Mode::Idle)
    }

    /// React to a keypress when no overlay is taking precedence.
    /// The keys we care about (Enter, Shift+Enter, Tab, Up/Down,
    /// Esc) are intercepted; everything else is forwarded to the
    /// inner `TextArea`. Completion-menu interactions land in
    /// `on_completion_key`; `tui::run` dispatches there before
    /// calling us.
    pub fn on_key(&mut self, key: KeyEvent) -> SubmitAction {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        // Transcript scroll keys. PgUp/PgDn always work; the
        // single-key vim shortcuts (`g`, `G`) and Home/End only
        // when the input box is empty so a user typing won't
        // accidentally scroll.
        match key.code {
            KeyCode::PageUp => {
                self.scroll_page_up();
                return SubmitAction::None;
            }
            KeyCode::PageDown => {
                self.scroll_page_down();
                return SubmitAction::None;
            }
            KeyCode::Home if ctrl => {
                self.scroll_to_top();
                return SubmitAction::None;
            }
            KeyCode::End if ctrl => {
                self.scroll_to_bottom();
                return SubmitAction::None;
            }
            _ => {}
        }
        // Bare `g` / `G` would conflict with typing those letters
        // as the first character of a prompt; bare Home / End
        // are useful at the start of a line for cursor movement.
        // Stick to PgUp/PgDn + Ctrl+Home/Ctrl+End — same surface
        // as `less`/most pagers.
        match key.code {
            KeyCode::Enter if shift || alt => {
                self.input.insert_newline();
                self.refresh_completion_on_edit();
            }
            KeyCode::Enter => return self.submit(),
            KeyCode::Tab if !ctrl && !alt => {
                self.open_or_advance_completion();
            }
            KeyCode::BackTab => {
                self.advance_completion(-1);
            }
            KeyCode::Up if !shift && !ctrl && !alt => {
                if self.is_single_line_buffer() {
                    self.history_prev();
                } else {
                    let _ = self.input.input(ta_input(key));
                }
            }
            KeyCode::Down if !shift && !ctrl && !alt => {
                if self.is_single_line_buffer() {
                    self.history_next();
                } else {
                    let _ = self.input.input(ta_input(key));
                }
            }
            KeyCode::Esc => {
                if self.completion.is_some() {
                    self.completion = None;
                } else {
                    self.clear_input();
                }
            }
            _ => {
                let changed = self.input.input(ta_input(key));
                if changed {
                    // Editing past the trigger boundary closes the
                    // popup; otherwise refresh candidates.
                    self.refresh_completion_on_edit();
                }
            }
        }
        SubmitAction::None
    }

    /// True when the user hasn't typed anything in the input box.
    /// Reserved for future first-keystroke detection (e.g. `?`
    /// hint) — currently unused; the bare-letter scroll
    /// shortcuts that needed it were dropped in favor of
    /// Ctrl+Home/Ctrl+End.
    #[allow(dead_code)]
    fn is_input_empty(&self) -> bool {
        self.input.lines().iter().all(|l| l.is_empty())
    }

    /// Keypress while the completion menu is open. `tui::run`
    /// dispatches here BEFORE `on_key` so menu navigation doesn't
    /// fall through to TextArea.
    pub fn on_completion_key(&mut self, key: KeyEvent) -> bool {
        let Some(menu) = self.completion.as_mut() else {
            return false;
        };
        match key.code {
            KeyCode::Up => {
                if menu.selected == 0 {
                    menu.selected = menu.candidates.len().saturating_sub(1);
                } else {
                    menu.selected -= 1;
                }
                true
            }
            KeyCode::Down => {
                if menu.candidates.is_empty() {
                    menu.selected = 0;
                } else {
                    menu.selected = (menu.selected + 1) % menu.candidates.len();
                }
                true
            }
            KeyCode::Tab => {
                self.advance_completion(1);
                true
            }
            KeyCode::BackTab => {
                self.advance_completion(-1);
                true
            }
            KeyCode::Enter => {
                self.accept_completion();
                true
            }
            KeyCode::Esc => {
                self.completion = None;
                true
            }
            _ => false,
        }
    }

    fn submit(&mut self) -> SubmitAction {
        if self.is_busy() {
            return SubmitAction::None;
        }
        if self.completion.is_some() {
            // Enter while popup is open is handled by
            // `on_completion_key` upstream; defensive no-op here.
            return SubmitAction::None;
        }
        let body = self.input.lines().join("\n");
        let trimmed = body.trim();
        if trimmed.is_empty() {
            return SubmitAction::None;
        }
        let body = trimmed.to_string();
        self.clear_input();
        self.history_cursor = None;
        self.history_draft = None;

        if body == ":q" {
            self.request_quit();
            return SubmitAction::None;
        }

        // Push to history, dedupe consecutive duplicates.
        if self.history.last().map(String::as_str) != Some(body.as_str()) {
            self.history.push(body.clone());
        }

        if let Some(rest) = body.strip_prefix('/') {
            return SubmitAction::Slash(rest.to_string());
        }
        self.transcript
            .push(TranscriptItem::UserPrompt { body: body.clone() });
        SubmitAction::Prompt(body)
    }

    fn clear_input(&mut self) {
        self.input = build_textarea();
        self.completion = None;
    }

    fn is_single_line_buffer(&self) -> bool {
        self.input.lines().len() == 1
    }

    // ---------- history ----------

    fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let pos = match self.history_cursor {
            Some(0) => return, // already at oldest
            Some(i) => i - 1,
            None => {
                // First Up press: save what the user was drafting
                // so Down past the end can restore it.
                self.history_draft = Some(self.input.lines().join("\n"));
                self.history.len() - 1
            }
        };
        self.set_input_text(&self.history[pos].clone());
        self.history_cursor = Some(pos);
    }

    fn history_next(&mut self) {
        match self.history_cursor {
            Some(i) if i + 1 < self.history.len() => {
                self.history_cursor = Some(i + 1);
                self.set_input_text(&self.history[i + 1].clone());
            }
            Some(_) => {
                // Past the newest entry: restore draft.
                let draft = self.history_draft.take().unwrap_or_default();
                self.set_input_text(&draft);
                self.history_cursor = None;
            }
            None => {}
        }
    }

    fn set_input_text(&mut self, text: &str) {
        // Replace the whole buffer. tui-textarea doesn't expose a
        // "set lines" — we rebuild from scratch and move the
        // cursor to the end so the user can keep typing.
        let lines: Vec<String> = text.split('\n').map(String::from).collect();
        self.input = build_textarea_with(lines);
        self.input.move_cursor(CursorMove::End);
        self.completion = None;
    }

    // ---------- completion ----------

    fn refresh_completion_on_edit(&mut self) {
        if self.completion.is_none() {
            return;
        }
        let ctx = self.detect_completion_context();
        match ctx {
            Some(new_ctx) => self.update_completion(new_ctx),
            None => self.completion = None,
        }
    }

    fn open_or_advance_completion(&mut self) {
        if self.completion.is_some() {
            self.advance_completion(1);
            return;
        }
        if let Some(ctx) = self.detect_completion_context() {
            self.update_completion(ctx);
        }
    }

    fn advance_completion(&mut self, delta: i32) {
        let Some(menu) = self.completion.as_mut() else {
            return;
        };
        if menu.candidates.is_empty() {
            return;
        }
        let n = menu.candidates.len();
        let i = menu.selected as i32 + delta;
        let i = ((i % n as i32) + n as i32) % n as i32;
        menu.selected = i as usize;
    }

    fn update_completion(&mut self, ctx: CompletionContext) {
        let candidates = match &ctx.kind {
            CompletionKind::Slash => completion::slash_candidates(&self.slash_names, &ctx.query),
            CompletionKind::Path { base_dir } => completion::path_candidates(base_dir, &ctx.query),
        };
        if candidates.is_empty() {
            self.completion = None;
            return;
        }
        let prior_selected = self
            .completion
            .as_ref()
            .map(|m| m.selected)
            .unwrap_or(0)
            .min(candidates.len() - 1);
        self.completion = Some(CompletionMenu {
            kind: ctx.kind,
            candidates,
            selected: prior_selected,
            query: ctx.query,
            replace_start_byte: ctx.replace_start_byte,
        });
    }

    fn accept_completion(&mut self) {
        let Some(menu) = self.completion.take() else {
            return;
        };
        let pick = match menu.candidates.get(menu.selected).cloned() {
            Some(s) => s,
            None => return,
        };
        // Replace from `replace_start_byte` (the trigger char's
        // position) to current cursor with the trigger + pick.
        let (row, col) = self.input.cursor();
        let line = self
            .input
            .lines()
            .get(row)
            .cloned()
            .unwrap_or_default();
        // Compute the byte index at the cursor (col is char-based
        // per tui-textarea).
        let cursor_byte = line
            .char_indices()
            .nth(col)
            .map(|(b, _)| b)
            .unwrap_or(line.len());
        let trigger = match menu.kind {
            CompletionKind::Slash => '/',
            CompletionKind::Path { .. } => '@',
        };
        let replacement = format!("{}{}", trigger, pick);
        // Walk back from the cursor by char count; tui-textarea's
        // delete_str takes a CHAR count, not a byte count.
        let chars_to_delete = line[menu.replace_start_byte..cursor_byte].chars().count();
        self.input.delete_str(chars_to_delete);
        self.input.insert_str(replacement);
        // Add a trailing space for slash completions so the user
        // can immediately type args. Path completions stop at the
        // selected entry — they may want to descend further (no
        // space).
        if matches!(self.completion, None) {
            // self.completion was just taken; check what it WAS
            // via the trigger:
            if trigger == '/' {
                self.input.insert_str(" ");
            }
        }
    }

    fn detect_completion_context(&self) -> Option<CompletionContext> {
        let (row, col) = self.input.cursor();
        let line = self.input.lines().get(row)?;
        let cursor_byte = line.char_indices().nth(col).map(|(b, _)| b).unwrap_or(line.len());
        completion::detect(line, cursor_byte, row == 0)
    }

    // ---------- driver-side event handlers ----------

    pub fn on_turn_started(&mut self) {
        self.mode = Mode::Thinking {
            since: Instant::now(),
        };
        self.transcript.push(TranscriptItem::Assistant {
            body: String::new(),
            done: false,
        });
        self.active_assistant = Some(self.transcript.len() - 1);
        self.note_arrival(2);
    }

    pub fn on_content_chunk(&mut self, chunk: &str) {
        if matches!(self.mode, Mode::Thinking { .. }) {
            self.mode = Mode::Streaming;
        }
        if self.active_assistant.is_none() {
            self.transcript.push(TranscriptItem::Assistant {
                body: String::new(),
                done: false,
            });
            self.active_assistant = Some(self.transcript.len() - 1);
        }
        if let Some(idx) = self.active_assistant {
            if let Some(TranscriptItem::Assistant { body, .. }) = self.transcript.get_mut(idx) {
                body.push_str(chunk);
            }
        }
        // Approximate by counting newlines in the chunk + 1 — a
        // single-token stream rarely carries multiple newlines.
        let n = chunk.matches('\n').count() as u16 + 1;
        self.note_arrival(n);
    }

    pub fn on_tool_start(&mut self, id: u64, tool: String, args_preview: String) {
        if let Some(idx) = self.active_assistant.take() {
            if let Some(TranscriptItem::Assistant { done, body, .. }) =
                self.transcript.get_mut(idx)
            {
                if !body.is_empty() {
                    *done = true;
                }
            }
        }
        self.transcript.push(TranscriptItem::ToolCard {
            id,
            tool,
            args_preview,
            state: ToolCardState::Running {
                started_at: Instant::now(),
            },
        });
        self.active_tools.insert(id, self.transcript.len() - 1);
        self.note_arrival(2);
    }

    pub fn on_tool_done(&mut self, id: u64, duration: Duration, summary: String, ok: bool) {
        if let Some(idx) = self.active_tools.remove(&id) {
            if let Some(TranscriptItem::ToolCard { state, .. }) = self.transcript.get_mut(idx) {
                *state = ToolCardState::Done {
                    duration,
                    summary,
                    ok,
                };
            }
        }
    }

    pub fn on_turn_finished(&mut self, _final_content: &str) {
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
        let lines = body.lines().count() as u16 + 1;
        self.transcript.push(TranscriptItem::System { body });
        self.note_arrival(lines);
    }

    pub fn on_approval_requested(
        &mut self,
        id: u64,
        tool: String,
        args: Value,
        reason: String,
    ) {
        self.approval = Some(ApprovalState {
            id,
            preview: crate::policy::preview_for(&tool, &args),
            tool,
            reason,
            scroll: 0,
        });
    }

    pub fn close_approval(&mut self) {
        self.approval = None;
    }

    pub fn approval_scroll_up(&mut self) {
        if let Some(a) = self.approval.as_mut() {
            a.scroll = a.scroll.saturating_sub(5);
        }
    }

    pub fn approval_scroll_down(&mut self) {
        if let Some(a) = self.approval.as_mut() {
            a.scroll = a.scroll.saturating_add(5);
        }
    }

    // ---------- transcript scroll ----------

    /// Called by `ui::draw` once per frame with the just-computed
    /// max-valid-offset and viewport height. Lets PgUp / PgDn
    /// step by a sensible amount and keeps `scroll_manual`
    /// clamped after a resize.
    pub fn note_scroll_metrics(&mut self, max: u16, viewport_height: u16) {
        self.scroll_max = max;
        self.scroll_viewport_height = viewport_height;
        if let Some(off) = self.scroll_manual.as_mut() {
            if *off > max {
                *off = max;
            }
        }
    }

    /// True when scrolling is detached from the bottom — i.e. the
    /// user has paged up and isn't seeing new content arrive.
    /// Drives the `↓ N new` indicator.
    pub fn is_scroll_detached(&self) -> bool {
        self.scroll_manual.is_some()
    }

    /// Account for newly-arrived content while detached. Called
    /// from the streaming/tool/system event handlers below.
    fn note_arrival(&mut self, lines_added: u16) {
        if self.scroll_manual.is_some() {
            self.unread_lines = self.unread_lines.saturating_add(lines_added);
        }
    }

    pub fn scroll_page_up(&mut self) {
        let step = self.scroll_viewport_height.saturating_sub(2).max(1);
        let current = self.scroll_manual.unwrap_or(self.scroll_max);
        let next = current.saturating_sub(step);
        self.scroll_manual = Some(next);
    }

    pub fn scroll_page_down(&mut self) {
        let step = self.scroll_viewport_height.saturating_sub(2).max(1);
        let current = self.scroll_manual.unwrap_or(self.scroll_max);
        let next = current.saturating_add(step);
        if next >= self.scroll_max {
            // Reached / passed the bottom — reattach so future
            // chunks auto-scroll.
            self.scroll_manual = None;
            self.unread_lines = 0;
        } else {
            self.scroll_manual = Some(next);
        }
    }

    /// Mouse wheel ticks scroll a few lines at a time — the
    /// terminal-native scroll feel.
    pub fn scroll_wheel_up(&mut self, lines: u16) {
        let current = self.scroll_manual.unwrap_or(self.scroll_max);
        self.scroll_manual = Some(current.saturating_sub(lines));
    }

    pub fn scroll_wheel_down(&mut self, lines: u16) {
        let current = self.scroll_manual.unwrap_or(self.scroll_max);
        let next = current.saturating_add(lines);
        if next >= self.scroll_max {
            self.scroll_manual = None;
            self.unread_lines = 0;
        } else {
            self.scroll_manual = Some(next);
        }
    }

    /// Jump to the very top of the transcript.
    pub fn scroll_to_top(&mut self) {
        self.scroll_manual = Some(0);
    }

    /// Jump to the bottom and reattach.
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_manual = None;
        self.unread_lines = 0;
    }

    pub fn take_cancel_sender(&mut self) -> Option<oneshot::Sender<()>> {
        self.cancel_tx.take()
    }

    pub fn set_cancel_sender(&mut self, tx: oneshot::Sender<()>) {
        self.cancel_tx = Some(tx);
    }
}

fn build_textarea() -> TextArea<'static> {
    build_textarea_with(vec![String::new()])
}

fn build_textarea_with(lines: Vec<String>) -> TextArea<'static> {
    let mut t = TextArea::new(lines);
    t.set_placeholder_text("type a message…  Shift+Enter for newline, Tab to complete");
    t
}

/// Convert a crossterm 0.29 `KeyEvent` to a tui-textarea `Input`.
/// tui-textarea 0.7 was built against crossterm 0.28; the From
/// impl that ships with it doesn't fire for our 0.29 type. Mapping
/// is straightforward — all the keys we care about have a 1:1
/// counterpart in `tui_textarea::Key`.
fn ta_input(key: KeyEvent) -> Input {
    let ta_key = match key.code {
        KeyCode::Char(c) => TaKey::Char(c),
        KeyCode::Backspace => TaKey::Backspace,
        KeyCode::Enter => TaKey::Enter,
        KeyCode::Left => TaKey::Left,
        KeyCode::Right => TaKey::Right,
        KeyCode::Up => TaKey::Up,
        KeyCode::Down => TaKey::Down,
        KeyCode::Home => TaKey::Home,
        KeyCode::End => TaKey::End,
        KeyCode::PageUp => TaKey::PageUp,
        KeyCode::PageDown => TaKey::PageDown,
        KeyCode::Tab => TaKey::Tab,
        KeyCode::BackTab => TaKey::Tab,
        KeyCode::Delete => TaKey::Delete,
        KeyCode::Esc => TaKey::Esc,
        KeyCode::F(n) => TaKey::F(n),
        _ => TaKey::Null,
    };
    Input {
        key: ta_key,
        ctrl: key.modifiers.contains(KeyModifiers::CONTROL),
        alt: key.modifiers.contains(KeyModifiers::ALT),
        shift: key.modifiers.contains(KeyModifiers::SHIFT),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn shift(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::SHIFT)
    }
    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }
    fn type_str(app: &mut App, s: &str) {
        for c in s.chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
    }
    fn input_string(app: &App) -> String {
        app.input.lines().join("\n")
    }

    #[test]
    fn typing_appends_to_input() {
        let mut app = App::new();
        type_str(&mut app, "hello");
        assert_eq!(input_string(&app), "hello");
    }

    #[test]
    fn shift_enter_inserts_newline_in_buffer() {
        let mut app = App::new();
        type_str(&mut app, "a");
        app.on_key(shift(KeyCode::Enter));
        type_str(&mut app, "b");
        assert_eq!(input_string(&app), "a\nb");
    }

    #[test]
    fn enter_submits_and_pushes_user_prompt() {
        let mut app = App::new();
        let starting = app.transcript.len();
        type_str(&mut app, "hello");
        let action = app.on_key(key(KeyCode::Enter));
        assert!(matches!(action, SubmitAction::Prompt(ref b) if b == "hello"));
        assert_eq!(app.transcript.len(), starting + 1);
        assert_eq!(input_string(&app), "");
    }

    #[test]
    fn enter_on_slash_returns_slash_action() {
        let mut app = App::new();
        type_str(&mut app, "/cost");
        let action = app.on_key(key(KeyCode::Enter));
        assert!(matches!(action, SubmitAction::Slash(ref b) if b == "cost"));
    }

    #[test]
    fn empty_or_whitespace_submission_is_a_noop() {
        let mut app = App::new();
        let starting = app.transcript.len();
        type_str(&mut app, "   ");
        let action = app.on_key(key(KeyCode::Enter));
        assert!(matches!(action, SubmitAction::None));
        assert_eq!(app.transcript.len(), starting);
    }

    #[test]
    fn esc_clears_input_when_no_completion_open() {
        let mut app = App::new();
        type_str(&mut app, "draft");
        app.on_key(key(KeyCode::Esc));
        assert_eq!(input_string(&app), "");
        assert!(!app.should_quit);
    }

    #[test]
    fn ctrl_c_keys_are_not_inserted_into_buffer() {
        // Global Ctrl+C/D handled by tui::run; on_key shouldn't
        // route them into the textarea. Today's TextArea ignores
        // Ctrl+C as a no-op (no copy when there's no selection)
        // so this is mostly a regression-canary.
        let mut app = App::new();
        app.on_key(ctrl('c'));
        app.on_key(ctrl('d'));
        assert_eq!(input_string(&app), "");
    }

    #[test]
    fn submit_pushes_into_history() {
        let mut app = App::new();
        type_str(&mut app, "first");
        app.on_key(key(KeyCode::Enter));
        type_str(&mut app, "second");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.history, vec!["first".to_string(), "second".to_string()]);
    }

    #[test]
    fn submit_dedupes_consecutive_duplicate_history_entries() {
        let mut app = App::new();
        type_str(&mut app, "abc");
        app.on_key(key(KeyCode::Enter));
        type_str(&mut app, "abc");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.history, vec!["abc".to_string()]);
    }

    #[test]
    fn up_arrow_walks_back_through_history_when_buffer_is_single_line() {
        let mut app = App::new();
        app.set_history(vec!["one".into(), "two".into(), "three".into()]);
        app.on_key(key(KeyCode::Up));
        assert_eq!(input_string(&app), "three");
        app.on_key(key(KeyCode::Up));
        assert_eq!(input_string(&app), "two");
        app.on_key(key(KeyCode::Up));
        assert_eq!(input_string(&app), "one");
        // Saturates at the oldest entry.
        app.on_key(key(KeyCode::Up));
        assert_eq!(input_string(&app), "one");
    }

    #[test]
    fn down_arrow_advances_through_history_and_restores_draft() {
        let mut app = App::new();
        app.set_history(vec!["one".into(), "two".into()]);
        type_str(&mut app, "draft");
        app.on_key(key(KeyCode::Up));
        assert_eq!(input_string(&app), "two");
        app.on_key(key(KeyCode::Down));
        // Past newest → restore draft.
        assert_eq!(input_string(&app), "draft");
    }

    #[test]
    fn up_in_a_multi_line_buffer_does_not_navigate_history() {
        let mut app = App::new();
        app.set_history(vec!["one".into()]);
        type_str(&mut app, "line one");
        app.on_key(shift(KeyCode::Enter));
        type_str(&mut app, "line two");
        app.on_key(key(KeyCode::Up));
        // History is NOT engaged; cursor moves within textarea.
        assert!(
            input_string(&app).contains("line one"),
            "expected unchanged buffer, got: {}",
            input_string(&app)
        );
        assert_eq!(input_string(&app), "line one\nline two");
    }

    #[test]
    fn approval_request_populates_modal_with_preview() {
        let mut app = App::new();
        let args = serde_json::json!({"file_path": "src/x.rs"});
        app.on_approval_requested(7, "Edit".into(), args, "edit src/x.rs".into());
        let approval = app.approval.expect("modal should be set");
        assert_eq!(approval.id, 7);
        assert_eq!(approval.tool, "Edit");
        assert!(approval.preview.contains("file: src/x.rs"));
    }

    #[test]
    fn close_approval_drops_the_modal() {
        let mut app = App::new();
        app.on_approval_requested(
            1,
            "Edit".into(),
            serde_json::json!({"file_path":"x"}),
            "r".into(),
        );
        app.close_approval();
        assert!(app.approval.is_none());
    }

    #[test]
    fn streaming_lifecycle_appends_chunks_to_active_assistant_item() {
        let mut app = App::new();
        let prior = app.transcript.len();
        app.on_turn_started();
        assert_eq!(app.transcript.len(), prior + 1);
        assert!(matches!(app.mode, Mode::Thinking { .. }));
        app.on_content_chunk("hello");
        assert!(matches!(app.mode, Mode::Streaming));
        app.on_content_chunk(" world");
        match &app.transcript[prior] {
            TranscriptItem::Assistant { body, done } => {
                assert_eq!(body, "hello world");
                assert!(!*done);
            }
            _ => panic!(),
        }
        app.on_turn_finished("hello world");
        assert!(matches!(app.mode, Mode::Idle));
    }

    #[test]
    fn tool_start_closes_active_assistant_and_pushes_running_card() {
        let mut app = App::new();
        app.on_turn_started();
        app.on_content_chunk("looking...");
        app.on_tool_start(1, "Read".into(), "file_path=x".into());
        assert!(app.active_assistant.is_none());
        match app.transcript.last().unwrap() {
            TranscriptItem::ToolCard { tool, state, .. } => {
                assert_eq!(tool, "Read");
                assert!(matches!(state, ToolCardState::Running { .. }));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn assistant_continuation_after_tool_creates_a_new_item() {
        let mut app = App::new();
        app.on_turn_started();
        app.on_content_chunk("first ");
        app.on_tool_start(1, "Read".into(), "x".into());
        app.on_tool_done(1, Duration::from_millis(1), "1 line".into(), true);
        app.on_content_chunk("second");
        let bodies: Vec<&str> = app
            .transcript
            .iter()
            .filter_map(|i| match i {
                TranscriptItem::Assistant { body, .. } => Some(body.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(bodies, vec!["first ", "second"]);
    }

    #[test]
    fn submitting_while_busy_returns_none() {
        let mut app = App::new();
        app.on_turn_started();
        type_str(&mut app, "hi");
        let action = app.on_key(key(KeyCode::Enter));
        assert!(matches!(action, SubmitAction::None));
    }

    // ---------- transcript scroll ----------

    #[test]
    fn page_up_detaches_from_bottom_and_decrements_offset() {
        let mut app = App::new();
        // Pretend the viewport is 10 rows tall and there are 100
        // logical lines — max valid offset = 90.
        app.note_scroll_metrics(90, 10);
        app.scroll_page_up();
        // After PgUp from "stuck": detached, offset = 90 - (10-2) = 82.
        assert_eq!(app.scroll_manual, Some(82));
        assert!(app.is_scroll_detached());
    }

    #[test]
    fn page_down_reattaches_when_reaching_bottom() {
        let mut app = App::new();
        app.note_scroll_metrics(20, 10);
        // Detach at offset 5, then PgDn until we hit max — should
        // re-attach (None) and zero unread.
        app.scroll_manual = Some(5);
        app.unread_lines = 7;
        app.scroll_page_down(); // 5 + 8 = 13 (still detached)
        assert_eq!(app.scroll_manual, Some(13));
        app.scroll_page_down(); // 13 + 8 = 21 >= 20 → reattach
        assert_eq!(app.scroll_manual, None);
        assert_eq!(app.unread_lines, 0);
    }

    #[test]
    fn ctrl_home_and_ctrl_end_navigate_to_top_and_bottom() {
        let mut app = App::new();
        app.note_scroll_metrics(40, 10);
        let ctrl_home = KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL);
        let ctrl_end = KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL);
        app.on_key(ctrl_home);
        assert_eq!(app.scroll_manual, Some(0));
        app.on_key(ctrl_end);
        assert_eq!(app.scroll_manual, None);
    }

    #[test]
    fn typing_g_or_uppercase_g_lands_in_buffer_not_scroll() {
        // We deliberately don't bind bare `g`/`G` — they have to
        // be available as the first letter of a prompt.
        let mut app = App::new();
        app.note_scroll_metrics(40, 10);
        type_str(&mut app, "g");
        assert_eq!(input_string(&app), "g");
        assert_eq!(app.scroll_manual, None);
    }

    #[test]
    fn unread_counter_grows_while_detached_and_resets_on_reattach() {
        let mut app = App::new();
        app.note_scroll_metrics(50, 10);
        app.scroll_manual = Some(10);
        // Simulate streaming arrival.
        app.on_content_chunk("hello world\nsecond line");
        assert!(app.unread_lines > 0);
        app.scroll_to_bottom();
        assert_eq!(app.unread_lines, 0);
    }

    #[test]
    fn note_scroll_metrics_clamps_offset_when_max_shrinks() {
        // A resize that reduces total lines should clamp the
        // user's offset to the new max.
        let mut app = App::new();
        app.scroll_manual = Some(80);
        app.note_scroll_metrics(50, 10);
        assert_eq!(app.scroll_manual, Some(50));
    }

    #[test]
    fn wheel_down_reattaches_at_bottom() {
        let mut app = App::new();
        app.note_scroll_metrics(30, 10);
        app.scroll_manual = Some(28);
        app.scroll_wheel_down(3);
        assert_eq!(app.scroll_manual, None);
    }
}
