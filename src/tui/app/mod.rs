//! `App` — in-memory state of one TUI session.
//!
//! Phase K replaced the hand-rolled single-line buffer with
//! `tui_textarea::TextArea`, added Up/Down + Ctrl-R history
//! navigation, and a completion-menu slot for slash and `@path`
//! popups.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::oneshot;
use tui_textarea::{CursorMove, Input, Key as TaKey, TextArea};

use crate::tui::completion::{self, CompletionContext};
use crate::tui::markdown::Theme as MarkdownTheme;
use crate::tui::theme::Theme;

mod overlay;
pub mod search;
mod transcript;

pub use overlay::{
    APPROVAL_OPTIONS, ApprovalState, CompletionKind, CompletionMenu, CopyFallbackState,
    HelpBrowserState, HistorySearchState, InlineHelpState, Overlay, SessionPickerRow,
    SessionsPickerState,
};
#[allow(unused_imports)]
pub use search::SearchState;
pub use transcript::{ToolCardState, TranscriptItem, committable_count};

pub enum Mode {
    Idle,
    Thinking { since: Instant },
    Streaming { since: Instant },
    ToolRunning { tool: String, since: Instant },
}

impl Default for Mode {
    fn default() -> Self {
        Mode::Idle
    }
}

pub struct App {
    pub transcript: Vec<TranscriptItem>,
    pub input: TextArea<'static>,
    pub should_quit: bool,
    pub mode: Mode,
    pub active_assistant: Option<usize>,
    pub cancel_tx: Option<oneshot::Sender<()>>,
    pub active_tools: HashMap<u64, usize>,
    /// The active modal overlay, if any. Mutually exclusive
    /// across approval, sessions picker, help browser, inline
    /// help, history search and wizard. Completion stays
    /// separate (it's an inline affordance, not modal).
    pub overlay: Option<Overlay>,

    /// Slash command names, sorted, mirroring the live registry
    /// in the driver. Updated at startup and on `/plugins reload`
    /// via `set_slash_names`.
    pub slash_names: Vec<String>,

    /// Open completion popup, if any. `None` when the user isn't
    /// currently completing.
    pub completion: Option<CompletionMenu>,

    /// `name → description` lookup mirroring the live slash
    /// registry. Lets `/help` and `/<cmd> ?` overlays render
    /// rich descriptions without round-tripping to the driver.
    pub slash_descriptions: HashMap<String, String>,
    /// Set of hint ids the user has already dismissed. Persisted
    /// across sessions in `~/.config/oli/tui-hints.json` so
    /// onboarding tips fade once the user has used the feature
    /// they describe.
    pub shown_hints: HashSet<String>,

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

    /// Light/dark mode hint for markdown / syntax-highlighted
    /// content. Detected from `$COLORFGBG` at TUI startup;
    /// defaults to dark on detection failure. Distinct from the
    /// full UI color palette (`theme`) — syntect needs only a
    /// binary light/dark choice.
    pub markdown_theme: MarkdownTheme,

    /// UI color palette. Resolved from `[ui].theme` at startup
    /// (see `tui::theme::load`). Render functions pull every
    /// semantic color from here so a theme swap recolors the
    /// entire surface coherently.
    pub theme: Theme,

    /// Status-bar fields. Identity (model / session / branch /
    /// ctx_window) is set once at startup; usage is updated by
    /// the driver after each chat round.
    pub status: StatusModel,

    /// Whether the host supports OSC52 clipboard writes. Resolved
    /// once at TUI startup from `Capabilities::osc52` + the
    /// `[ui].osc52` override (Phase W4). When false, `/copy N`
    /// opens the `Overlay::CopyFallback` modal instead of writing
    /// the escape into a host that would silently drop it.
    pub osc52_supported: bool,

    /// Short identifier of the host terminal (`caps.host`), shown
    /// in the copy-fallback modal title so the user knows *which*
    /// host blocked OSC52. Set once at startup.
    pub host_hint: String,

    /// Count of matches the transcript renderer found for the
    /// current search query at the last paint. The renderer is
    /// the authority on match positions (it walks the laid-out
    /// lines), so the key handler reads this cached count when
    /// the user hits `n` / `N` to cycle.
    pub search_match_count: usize,

    /// Line indices (post-layout) of the start of each user turn.
    /// Same renderer-writes / handler-reads contract as
    /// `search_match_count`. Drives `[` / `]` turn-jump nav (X3).
    pub turn_line_indices: Vec<u16>,

    /// Position-stack history for Ctrl+O (back) / Ctrl+I (forward).
    /// Each entry is a `scroll_manual` value (None = attached to
    /// bottom). The cursor points one *past* the current position;
    /// new positions are pushed at the cursor (truncating any
    /// forward history). Capped at SCROLL_HISTORY_CAP entries.
    pub scroll_positions: Vec<Option<u16>>,
    pub scroll_pos_cursor: usize,

    /// Phase Y4: transcript index of the currently focused tool
    /// card (Done state). None = no card focused. `{` / `}` cycle
    /// among Done cards; `Enter` (on empty input) toggles the
    /// focused card's `expanded` flag; `Esc` clears focus.
    pub focused_card_idx: Option<usize>,

    /// Inline-mode scrollback watermark: count of leading transcript
    /// items already flushed to native scrollback via
    /// `Terminal::insert_before`. Items `[0, committed)` live in the
    /// host's scrollback; `[committed, len)` render in the viewport.
    /// Stays `0` in fullscreen mode (the commit step never runs), so
    /// the viewport renders the whole transcript exactly as before.
    pub committed: usize,

    /// Wall-clock start of the current turn, captured in
    /// `on_turn_started`. Feeds the `Worked for …` label on the
    /// turn separator emitted at `on_turn_finished`.
    pub turn_started_at: Option<Instant>,
}

/// Position-stack depth for Ctrl+O / Ctrl+I jumps. The spec's
/// "Done when" wants a stack of ≥ 8 entries.
pub const SCROLL_HISTORY_CAP: usize = 16;

/// Identity fields for the footer and welcome box. Optional
/// fields are dropped from the footer when the terminal narrows.
#[derive(Clone, Debug, Default)]
pub struct StatusModel {
    pub session_id: Option<String>,
    pub model: String,
    pub ctx_window: u32,
    pub branch: Option<String>,
    /// `~`-relativized cwd for the footer.
    pub cwd: String,
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
            overlay: None,
            slash_names: Vec::new(),
            completion: None,
            history: Vec::new(),
            history_cursor: None,
            history_draft: None,
            scroll_manual: None,
            unread_lines: 0,
            scroll_max: 0,
            scroll_viewport_height: 0,
            markdown_theme: MarkdownTheme::Dark,
            theme: Theme::dark(),
            status: StatusModel::default(),
            slash_descriptions: HashMap::new(),
            shown_hints: HashSet::new(),
            osc52_supported: true,
            host_hint: String::from("unknown"),
            search_match_count: 0,
            turn_line_indices: Vec::new(),
            scroll_positions: Vec::new(),
            scroll_pos_cursor: 0,
            focused_card_idx: None,
            committed: 0,
            turn_started_at: None,
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
        app.markdown_theme = MarkdownTheme::detect();
        app.transcript.push(TranscriptItem::Welcome);
        app
    }

    /// Replace the (name, description) lookup. Updates
    /// `slash_names` too so completion still works without an
    /// extra call.
    pub fn set_slash_meta(&mut self, mut meta: Vec<(String, String)>) {
        meta.sort_by(|a, b| a.0.cmp(&b.0));
        self.slash_names = meta.iter().map(|(n, _)| n.clone()).collect();
        self.slash_descriptions = meta.into_iter().collect();
    }

    pub fn set_shown_hints(&mut self, hints: HashSet<String>) {
        self.shown_hints = hints;
    }

    /// Mark a hint id as shown — fading the corresponding tip
    /// from future renders. Caller persists the updated set to
    /// disk (the App stays pure).
    pub fn mark_hint_shown(&mut self, id: &str) {
        self.shown_hints.insert(id.to_string());
    }

    pub fn hint_is_unseen(&self, id: &str) -> bool {
        !self.shown_hints.contains(id)
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

    /// Wire the OSC52 capability + host label set at TUI startup.
    /// The slash handler reads these to pick between writing the
    /// OSC52 escape or opening the copy-fallback modal.
    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
    }

    pub fn set_clipboard_caps(&mut self, osc52_supported: bool, host_hint: String) {
        self.osc52_supported = osc52_supported;
        self.host_hint = host_hint;
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
                self.record_scroll_position();
                self.scroll_to_top();
                return SubmitAction::None;
            }
            KeyCode::End if ctrl => {
                self.record_scroll_position();
                self.scroll_to_bottom();
                return SubmitAction::None;
            }
            // Turn-jump nav (X3): `[` / `]` jump between user
            // turns, but only when the input is empty so the
            // characters can still be typed mid-prompt.
            KeyCode::Char('[') if !ctrl && !alt && self.is_input_empty() => {
                self.jump_to_prev_turn();
                return SubmitAction::None;
            }
            KeyCode::Char(']') if !ctrl && !alt && self.is_input_empty() => {
                self.jump_to_next_turn();
                return SubmitAction::None;
            }
            // Card-focus nav (Y4): `{` / `}` cycle among Done
            // tool cards. Same empty-input guard as `[` / `]` so
            // typing those characters mid-prompt still works.
            KeyCode::Char('{') if !ctrl && !alt && self.is_input_empty() => {
                self.focus_prev_card();
                return SubmitAction::None;
            }
            KeyCode::Char('}') if !ctrl && !alt && self.is_input_empty() => {
                self.focus_next_card();
                return SubmitAction::None;
            }
            KeyCode::Char('?')
                if self.input.lines().len() == 1 && self.input.lines()[0].is_empty() =>
            {
                self.open_help_browser();
                return SubmitAction::None;
            }
            // Position-stack jumps (X3): Ctrl+O steps back through
            // recorded scroll positions, Ctrl+I steps forward. In
            // legacy terminals Ctrl+I is indistinguishable from
            // Tab; only kitty-keyboard-mode terminals see the
            // distinct event.
            KeyCode::Char('o') if ctrl => {
                self.jump_back_in_history();
                return SubmitAction::None;
            }
            KeyCode::Char('i') if ctrl => {
                self.jump_forward_in_history();
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
            // Y4: Enter with a focused card and empty input toggles
            // expand/collapse on the card. Falls through to submit
            // when input has content (so a focused card doesn't
            // hijack the natural prompt-submit flow).
            KeyCode::Enter if self.focused_card_idx.is_some() && self.is_input_empty() => {
                self.toggle_focused_card_expanded();
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
                } else if self.focused_card_idx.is_some() {
                    // Y4: clear card focus first; a second Esc still
                    // clears the input as before.
                    self.clear_card_focus();
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
    /// Gates the bare-letter shortcuts (`[`/`]` turn-jump nav)
    /// so they don't steal keystrokes mid-prompt.
    pub fn is_input_empty(&self) -> bool {
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
        // Auto-open as the user types: if the cursor is in a
        // completion context (`/cmd` at line start, `@path` after
        // a word boundary), surface the popup without waiting for
        // Tab. Tab still works for explicit invocation.
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
        let match_positions: Vec<Vec<u32>> = if ctx.query.is_empty() {
            candidates.iter().map(|_| Vec::new()).collect()
        } else {
            candidates
                .iter()
                .map(|c| crate::tui::fuzzy::match_positions(&ctx.query, c))
                .collect()
        };
        let prior_selected = self
            .completion
            .as_ref()
            .map(|m| m.selected)
            .unwrap_or(0)
            .min(candidates.len() - 1);
        self.completion = Some(CompletionMenu {
            kind: ctx.kind,
            candidates,
            match_positions,
            selected: prior_selected,
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
        // Replace `[replace_start_byte .. cursor]` with `trigger + pick`.
        // tui-textarea's `delete_str` deletes FORWARD from the
        // cursor, so we jump the cursor back to the trigger
        // position first, then delete the typed prefix forward,
        // then insert the replacement.
        let (row, col) = self.input.cursor();
        let line = self.input.lines().get(row).cloned().unwrap_or_default();
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
        let chars_to_delete = line[menu.replace_start_byte..cursor_byte].chars().count();
        // Char-based column at the trigger position.
        let start_col = line[..menu.replace_start_byte].chars().count();
        self.input
            .move_cursor(CursorMove::Jump(row as u16, start_col as u16));
        self.input.delete_str(chars_to_delete);
        self.input.insert_str(replacement);
        // Add a trailing space for slash completions so the user
        // can immediately type args. Path completions stop at the
        // selected entry — they may want to descend further (no
        // space).
        if trigger == '/' {
            self.input.insert_str(" ");
        }
    }

    fn detect_completion_context(&self) -> Option<CompletionContext> {
        let (row, col) = self.input.cursor();
        let line = self.input.lines().get(row)?;
        let cursor_byte = line
            .char_indices()
            .nth(col)
            .map(|(b, _)| b)
            .unwrap_or(line.len());
        completion::detect(line, cursor_byte, row == 0)
    }

    /// Replace the input buffer's content. Used by Ctrl+E so the
    /// user can re-edit a previous prompt after `/undo` removes
    /// it.
    pub fn set_input_text_pub(&mut self, text: &str) {
        self.set_input_text(text);
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
    t.set_placeholder_text("Ask oli to do anything");
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
mod tests;
