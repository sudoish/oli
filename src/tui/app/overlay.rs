//! Overlay state types — the modal UI surfaces App can have
//! open and the in-input completion menu — and the `impl App`
//! block that opens, closes and navigates each overlay.
//!
//! Only one `Overlay` variant is live at a time; the keypress
//! router in `tui/mod.rs` matches on the active variant before
//! delegating to per-overlay key handlers. Completion sits
//! alongside (it's an inline affordance, not modal).

use std::path::PathBuf;

use super::App;
use super::search::SearchState;

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
    /// Char-index positions inside each candidate's displayed label
    /// (`candidates[i]`) that contributed to the fuzzy match. Used
    /// by the popup renderer to highlight matched characters.
    /// Empty inner vec = no highlight (empty query or no match info).
    pub match_positions: Vec<Vec<u32>>,
    pub selected: usize,
    /// Byte offset on the active line where the trigger char
    /// (`/` or `@`) lives. The substring from there to the
    /// cursor is what gets replaced when the user accepts.
    pub replace_start_byte: usize,
}

#[derive(Debug, Clone)]
pub struct ApprovalState {
    pub tool: String,
    pub reason: String,
    pub preview: String,
    pub scroll: u16,
    /// Cursor into `APPROVAL_OPTIONS` for the inline option list.
    pub selected: usize,
}

/// `(label, key)` — the five approval responses in list order. The
/// list index maps to an `ApprovalResponse` via
/// `crate::tui::event::approval_response_for`.
pub const APPROVAL_OPTIONS: [(&str, &str); 5] = [
    ("Yes", "y"),
    ("No", "n"),
    ("Allow for this session", "a"),
    ("Allow always, persisted", "A"),
    ("Deny for this session", "d"),
];

/// `/sessions` modal. Lists session entries, newest first.
/// Selection is by index in `entries`. Enter triggers a copy of
/// the resume command + a system-note hint; Esc closes.
#[derive(Debug, Clone)]
pub struct SessionsPickerState {
    pub entries: Vec<SessionPickerRow>,
    pub selected: usize,
}

#[derive(Debug, Clone)]
pub struct SessionPickerRow {
    pub id: String,
    pub label: String,
}

/// `/help` browser. Two-pane: list on the left, full description
/// of the highlighted command on the right. Esc / Enter closes.
#[derive(Debug, Clone)]
pub struct HelpBrowserState {
    pub entries: Vec<(String, String)>,
    pub selected: usize,
}

/// `/<cmd> ?` one-shot card. Lifetime is "until the next
/// keystroke" — any key dismisses, except modifier-only events.
#[derive(Debug, Clone)]
pub struct InlineHelpState {
    pub name: String,
    pub description: String,
}

/// `/copy N` fallback overlay. Shown instead of writing the OSC52
/// escape when the host doesn't support it (Neovim `:terminal`,
/// VSCode integrated terminal, generic xterm without OSC52
/// allowlisted). The body is the verbatim assistant message; the
/// user selects + copies via the host's normal selection
/// affordances and dismisses with any key.
#[derive(Debug, Clone)]
pub struct CopyFallbackState {
    /// Verbatim message body. Rendered as-is, no markdown reflow,
    /// so what the user copies matches what they would have got
    /// from OSC52.
    pub body: String,
    /// 1-based index of the assistant message the user asked to
    /// copy (the `N` from `/copy N`). Displayed in the title.
    pub index: usize,
    /// Reason the fallback opened — usually `caps.host` from
    /// Capabilities, plus a short note. Drives the title hint so
    /// the user knows *why* the modal opened.
    pub host_hint: String,
    /// Scroll offset within the body, advanced by PgUp/PgDn so
    /// long messages stay readable inside a small modal.
    pub scroll: u16,
}

/// Ctrl-R history search overlay. Substring match (case-
/// insensitive); newest matches first; arrow keys navigate;
/// Enter loads the picked entry into the input; Esc cancels.
#[derive(Debug, Clone, Default)]
pub struct HistorySearchState {
    pub query: String,
    /// Indices into `App.history`, newest-first, filtered by
    /// `query`. Recomputed on every keystroke.
    pub matches: Vec<usize>,
    pub selected: usize,
}

/// The exclusive set of modal overlays the TUI can have open.
/// Only one is up at a time — pressing Esc / Enter / etc. on any
/// of them closes that overlay before the next one can open.
/// Completion is *not* in here: it's an in-input affordance, not
/// modal, and lives on its own field.
#[derive(Debug, Clone)]
pub enum Overlay {
    Approval(ApprovalState),
    SessionsPicker(SessionsPickerState),
    HelpBrowser(HelpBrowserState),
    InlineHelp(InlineHelpState),
    HistorySearch(HistorySearchState),
    CopyFallback(CopyFallbackState),
    Wizard(crate::tui::wizard::WizardState),
    /// In-transcript search bar (Ctrl+F). Substring (case-
    /// insensitive) match against the rendered transcript;
    /// Enter / n / N cycle matches; Esc closes.
    Search(SearchState),
}

impl App {
    // ---------- overlay accessors ----------
    //
    // The overlay enum centralizes all six modal states. These
    // typed accessors keep call sites ergonomic — most existing
    // call sites just rename `app.approval.is_some()` to
    // `app.approval().is_some()` and similar.

    pub fn approval(&self) -> Option<&ApprovalState> {
        match &self.overlay {
            Some(Overlay::Approval(s)) => Some(s),
            _ => None,
        }
    }

    pub fn approval_mut(&mut self) -> Option<&mut ApprovalState> {
        match &mut self.overlay {
            Some(Overlay::Approval(s)) => Some(s),
            _ => None,
        }
    }

    pub fn sessions_picker(&self) -> Option<&SessionsPickerState> {
        match &self.overlay {
            Some(Overlay::SessionsPicker(s)) => Some(s),
            _ => None,
        }
    }

    /// Read-only accessor for the active `/help` browser, if
    /// any. Tests reach for this directly; production code uses
    /// the render-time `match &app.overlay` instead.
    #[cfg(test)]
    pub fn help_browser(&self) -> Option<&HelpBrowserState> {
        match &self.overlay {
            Some(Overlay::HelpBrowser(s)) => Some(s),
            _ => None,
        }
    }

    /// Test-only accessor for the active inline-help card.
    #[cfg(test)]
    pub fn inline_help(&self) -> Option<&InlineHelpState> {
        match &self.overlay {
            Some(Overlay::InlineHelp(s)) => Some(s),
            _ => None,
        }
    }

    pub fn history_search(&self) -> Option<&HistorySearchState> {
        match &self.overlay {
            Some(Overlay::HistorySearch(s)) => Some(s),
            _ => None,
        }
    }

    pub fn wizard(&self) -> Option<&crate::tui::wizard::WizardState> {
        match &self.overlay {
            Some(Overlay::Wizard(s)) => Some(s),
            _ => None,
        }
    }

    pub fn wizard_mut(&mut self) -> Option<&mut crate::tui::wizard::WizardState> {
        match &mut self.overlay {
            Some(Overlay::Wizard(s)) => Some(s),
            _ => None,
        }
    }

    // ---------- open / close / navigate ----------

    /// `/help` browser opener. Snapshots the current slash list
    /// so the modal doesn't churn when the registry changes.
    pub fn open_help_browser(&mut self) {
        let mut entries: Vec<(String, String)> =
            self.slash_descriptions.clone().into_iter().collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        self.overlay = Some(Overlay::HelpBrowser(HelpBrowserState {
            entries,
            selected: 0,
        }));
    }

    pub fn close_help_browser(&mut self) {
        if matches!(self.overlay, Some(Overlay::HelpBrowser(_))) {
            self.overlay = None;
        }
    }

    pub fn help_browser_navigate(&mut self, delta: i32) {
        if let Some(Overlay::HelpBrowser(b)) = &mut self.overlay {
            if b.entries.is_empty() {
                return;
            }
            let n = b.entries.len() as i32;
            let next = (b.selected as i32 + delta).rem_euclid(n);
            b.selected = next as usize;
        }
    }

    /// `/sessions` picker opener. `entries` is what the binary
    /// has already gathered via `list_sessions()` — we don't
    /// reach into the filesystem from App.
    pub fn open_sessions_picker(&mut self, entries: Vec<SessionPickerRow>) {
        self.overlay = Some(Overlay::SessionsPicker(SessionsPickerState {
            entries,
            selected: 0,
        }));
    }

    pub fn close_sessions_picker(&mut self) {
        if matches!(self.overlay, Some(Overlay::SessionsPicker(_))) {
            self.overlay = None;
        }
    }

    pub fn sessions_picker_navigate(&mut self, delta: i32) {
        if let Some(Overlay::SessionsPicker(p)) = &mut self.overlay {
            if p.entries.is_empty() {
                return;
            }
            let n = p.entries.len() as i32;
            let next = (p.selected as i32 + delta).rem_euclid(n);
            p.selected = next as usize;
        }
    }

    /// Pull the highlighted session id out of the picker (or
    /// `None` if the picker is empty / closed).
    pub fn sessions_picker_pick(&self) -> Option<String> {
        let p = self.sessions_picker()?;
        p.entries.get(p.selected).map(|r| r.id.clone())
    }

    pub fn open_wizard(&mut self) {
        self.overlay = Some(Overlay::Wizard(crate::tui::wizard::WizardState::new()));
    }

    pub fn close_wizard(&mut self) {
        if matches!(self.overlay, Some(Overlay::Wizard(_))) {
            self.overlay = None;
        }
    }

    pub fn open_history_search(&mut self) {
        let mut state = HistorySearchState::default();
        state.matches = self.history_search_compute_matches("");
        self.overlay = Some(Overlay::HistorySearch(state));
    }

    pub fn close_history_search(&mut self) {
        if matches!(self.overlay, Some(Overlay::HistorySearch(_))) {
            self.overlay = None;
        }
    }

    pub fn history_search_push_char(&mut self, c: char) {
        let Some(Overlay::HistorySearch(s)) = &self.overlay else {
            return;
        };
        let mut new_query = s.query.clone();
        new_query.push(c);
        let matches = self.history_search_compute_matches(&new_query);
        if let Some(Overlay::HistorySearch(s)) = &mut self.overlay {
            s.query = new_query;
            s.matches = matches;
            s.selected = 0;
        }
    }

    pub fn history_search_backspace(&mut self) {
        let Some(Overlay::HistorySearch(s)) = &self.overlay else {
            return;
        };
        let mut new_query = s.query.clone();
        new_query.pop();
        let matches = self.history_search_compute_matches(&new_query);
        if let Some(Overlay::HistorySearch(s)) = &mut self.overlay {
            s.query = new_query;
            s.matches = matches;
            s.selected = 0;
        }
    }

    pub fn history_search_navigate(&mut self, delta: i32) {
        if let Some(Overlay::HistorySearch(s)) = &mut self.overlay {
            if s.matches.is_empty() {
                return;
            }
            let n = s.matches.len() as i32;
            let next = (s.selected as i32 + delta).rem_euclid(n);
            s.selected = next as usize;
        }
    }

    /// Pick the highlighted entry's body (or `None` if no
    /// matches). Used by the Enter handler in `tui::mod` to
    /// load it into the input box.
    pub fn history_search_pick(&self) -> Option<String> {
        let s = self.history_search()?;
        let idx = *s.matches.get(s.selected)?;
        self.history.get(idx).cloned()
    }

    fn history_search_compute_matches(&self, query: &str) -> Vec<usize> {
        let q = query.to_ascii_lowercase();
        // Newest-first iteration over indices.
        let mut out: Vec<usize> = (0..self.history.len()).rev().collect();
        if !q.is_empty() {
            out.retain(|&i| self.history[i].to_ascii_lowercase().contains(&q));
        }
        // Cap so the popup stays small even on huge histories.
        out.truncate(50);
        out
    }

    /// Open the `/copy N` fallback modal with the verbatim body
    /// the user asked to copy. `host_hint` is the short label
    /// (`caps.host`) so the title can explain *why* the fallback
    /// opened. Closes any prior overlay first.
    pub fn open_copy_fallback(&mut self, body: String, index: usize, host_hint: String) {
        self.overlay = Some(Overlay::CopyFallback(CopyFallbackState {
            body,
            index,
            host_hint,
            scroll: 0,
        }));
    }

    pub fn close_copy_fallback(&mut self) {
        if matches!(self.overlay, Some(Overlay::CopyFallback(_))) {
            self.overlay = None;
        }
    }

    /// Test-only accessor for the active copy-fallback modal.
    #[cfg(test)]
    pub fn copy_fallback(&self) -> Option<&CopyFallbackState> {
        match &self.overlay {
            Some(Overlay::CopyFallback(s)) => Some(s),
            _ => None,
        }
    }

    pub fn copy_fallback_scroll_up(&mut self) {
        if let Some(Overlay::CopyFallback(s)) = &mut self.overlay {
            s.scroll = s.scroll.saturating_sub(5);
        }
    }

    pub fn copy_fallback_scroll_down(&mut self) {
        if let Some(Overlay::CopyFallback(s)) = &mut self.overlay {
            s.scroll = s.scroll.saturating_add(5);
        }
    }

    // ---------- in-transcript search ----------

    pub fn search(&self) -> Option<&SearchState> {
        match &self.overlay {
            Some(Overlay::Search(s)) => Some(s),
            _ => None,
        }
    }

    pub fn open_search(&mut self) {
        self.overlay = Some(Overlay::Search(SearchState::default()));
    }

    pub fn close_search(&mut self) {
        if matches!(self.overlay, Some(Overlay::Search(_))) {
            self.overlay = None;
        }
    }

    pub fn search_push_char(&mut self, c: char) {
        if let Some(Overlay::Search(s)) = &mut self.overlay {
            s.query.push(c);
            s.current = 0;
        }
    }

    pub fn search_backspace(&mut self) {
        if let Some(Overlay::Search(s)) = &mut self.overlay {
            s.query.pop();
            s.current = 0;
        }
    }

    /// Step the focused match forward (`+1`) or backward (`-1`).
    /// `match_count` is supplied by the renderer, which knows
    /// how many lines matched the current query. No-op when zero.
    pub fn search_navigate(&mut self, delta: i32, match_count: usize) {
        if match_count == 0 {
            return;
        }
        if let Some(Overlay::Search(s)) = &mut self.overlay {
            let n = match_count as i32;
            let next = (s.current as i32 + delta).rem_euclid(n);
            s.current = next as usize;
        }
    }

    pub fn open_inline_help(&mut self, name: &str) {
        let description = self
            .slash_descriptions
            .get(name)
            .cloned()
            .unwrap_or_else(|| format!("(no help registered for /{})", name));
        self.overlay = Some(Overlay::InlineHelp(InlineHelpState {
            name: name.to_string(),
            description,
        }));
    }

    pub fn close_inline_help(&mut self) {
        if matches!(self.overlay, Some(Overlay::InlineHelp(_))) {
            self.overlay = None;
        }
    }

    pub fn on_approval_requested(&mut self, tool: String, args: serde_json::Value, reason: String) {
        self.overlay = Some(Overlay::Approval(ApprovalState {
            preview: crate::policy::preview_for(&tool, &args),
            tool,
            reason,
            scroll: 0,
            selected: 0,
        }));
    }

    pub fn close_approval(&mut self) {
        if matches!(self.overlay, Some(Overlay::Approval(_))) {
            self.overlay = None;
        }
    }

    pub fn approval_scroll_up(&mut self) {
        if let Some(a) = self.approval_mut() {
            a.scroll = a.scroll.saturating_sub(5);
        }
    }

    pub fn approval_scroll_down(&mut self) {
        if let Some(a) = self.approval_mut() {
            a.scroll = a.scroll.saturating_add(5);
        }
    }

    pub fn approval_select_prev(&mut self) {
        if let Some(a) = self.approval_mut() {
            a.selected = a.selected.saturating_sub(1);
        }
    }

    pub fn approval_select_next(&mut self) {
        if let Some(a) = self.approval_mut() {
            a.selected = (a.selected + 1).min(APPROVAL_OPTIONS.len() - 1);
        }
    }
}
