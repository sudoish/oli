//! Transcript items — the message log the TUI renders. Stream-
//! ing assistant content, user prompts, system notes, and tool
//! cards all live here. Also hosts the `impl App` block with the
//! driver-side event handlers (on_turn_*, on_tool_*, etc.) and
//! the transcript scroll math.

use std::time::{Duration, Instant};

use super::{App, Mode};

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

impl App {
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
        // First chunk of any tool/thinking phase flips to Streaming.
        // We always reset `since` so the activity strip's elapsed
        // counter reflects time-in-this-stream, not time-since-turn.
        if !matches!(self.mode, Mode::Streaming { .. }) {
            self.mode = Mode::Streaming {
                since: Instant::now(),
            };
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
        let started_at = Instant::now();
        self.mode = Mode::ToolRunning {
            tool: tool.clone(),
            since: started_at,
        };
        self.transcript.push(TranscriptItem::ToolCard {
            tool,
            args_preview,
            state: ToolCardState::Running { started_at },
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
        // Tool finished — agent will be processing the result before
        // the next stream resumes. Reflect that with Thinking; the
        // next content chunk flips it to Streaming.
        if matches!(self.mode, Mode::ToolRunning { .. }) && self.active_tools.is_empty() {
            self.mode = Mode::Thinking {
                since: Instant::now(),
            };
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

    pub fn on_slash_finished(&mut self) {
        self.mode = Mode::Idle;
        self.cancel_tx = None;
    }

    /// Drop the most recent UserPrompt and every transcript item
    /// that came after it. Returns the body of the popped prompt
    /// for the caller (e.g. so Ctrl+E edit-and-rerun can re-load
    /// it into the input box). Active-assistant / active-tool
    /// indices are reset since the items they pointed at may be
    /// gone.
    pub fn undo_last_user_turn(&mut self) -> Option<String> {
        let last_user_idx = self
            .transcript
            .iter()
            .rposition(|i| matches!(i, TranscriptItem::UserPrompt { .. }))?;
        let body = match &self.transcript[last_user_idx] {
            TranscriptItem::UserPrompt { body } => body.clone(),
            _ => unreachable!(),
        };
        self.transcript.truncate(last_user_idx);
        self.active_assistant = None;
        self.active_tools.clear();
        Some(body)
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
    /// from the streaming/tool/system event handlers above.
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

    /// Scroll so the previous user-turn header sits at (or near)
    /// the top of the visible region. Uses
    /// `turn_line_indices` cached by the transcript renderer.
    /// No-op if there are no user turns or none lies above the
    /// current view.
    pub fn jump_to_prev_turn(&mut self) {
        let current = self.scroll_manual.unwrap_or(self.scroll_max);
        let target = self
            .turn_line_indices
            .iter()
            .rev()
            .copied()
            .find(|idx| *idx < current);
        if let Some(idx) = target {
            self.scroll_manual = Some(idx.min(self.scroll_max));
        }
    }

    /// Counterpart of `jump_to_prev_turn`. Reattaches (scroll_manual
    /// = None) when the next-turn target sits at or below the
    /// natural bottom of the view.
    pub fn jump_to_next_turn(&mut self) {
        let current = self.scroll_manual.unwrap_or(self.scroll_max);
        let target = self
            .turn_line_indices
            .iter()
            .copied()
            .find(|idx| *idx > current);
        if let Some(idx) = target {
            if idx >= self.scroll_max {
                self.scroll_manual = None;
                self.unread_lines = 0;
            } else {
                self.scroll_manual = Some(idx);
            }
        }
    }
}
