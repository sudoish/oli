//! Tests for the `app` module — extracted so `mod.rs` stays
//! focused on production code. Brought in via
//! `#[cfg(test)] mod tests;` from mod.rs, so the file's
//! contents form the body of that module and have full access
//! to private items via `use super::*;`.

use super::*;
use crossterm::event::KeyCode;
use std::time::{Duration, Instant};

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
fn typing_slash_auto_opens_completion_popup() {
    let mut app = App::new();
    app.set_slash_meta(vec![
        ("clear".into(), String::new()),
        ("cost".into(), String::new()),
        ("model".into(), String::new()),
    ]);
    type_str(&mut app, "/c");
    let menu = app.completion.as_ref().expect("popup should auto-open");
    let mut got = menu.candidates.clone();
    got.sort();
    assert_eq!(got, vec!["clear".to_string(), "cost".to_string()]);
}

#[test]
fn accepting_completion_replaces_typed_prefix_not_appends_to_it() {
    // Regression: typing /mod then accepting /model used to produce
    // "/mod/model" because delete_str was deleting forward from a
    // cursor that sat at end-of-input.
    let mut app = App::new();
    app.set_slash_meta(vec![
        ("model".into(), String::new()),
        ("mode".into(), String::new()),
    ]);
    type_str(&mut app, "/mod");
    assert!(app.completion.is_some(), "popup should be open");
    // Accept whatever's first in the menu (alphabetically: mode).
    app.on_completion_key(key(KeyCode::Enter));
    let buf = input_string(&app);
    assert!(
        buf == "/mode " || buf == "/model ",
        "expected /<pick> with trailing space, got {:?}",
        buf
    );
}

#[test]
fn typing_past_slash_args_closes_completion_popup() {
    let mut app = App::new();
    app.set_slash_meta(vec![("model".into(), String::new())]);
    type_str(&mut app, "/model");
    assert!(app.completion.is_some());
    type_str(&mut app, " arg");
    assert!(app.completion.is_none());
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
    app.on_approval_requested("Edit".into(), args, "edit src/x.rs".into());
    let approval = app.approval().expect("modal should be set");
    assert_eq!(approval.tool, "Edit");
    assert!(approval.preview.contains("file: src/x.rs"));
}

#[test]
fn close_approval_drops_the_modal() {
    let mut app = App::new();
    app.on_approval_requested(
        "Edit".into(),
        serde_json::json!({"file_path":"x"}),
        "r".into(),
    );
    app.close_approval();
    assert!(app.approval().is_none());
}

#[test]
fn streaming_lifecycle_appends_chunks_to_active_assistant_item() {
    let mut app = App::new();
    let prior = app.transcript.len();
    app.on_turn_started();
    assert_eq!(app.transcript.len(), prior + 1);
    assert!(matches!(app.mode, Mode::Thinking { .. }));
    app.on_content_chunk("hello");
    assert!(matches!(app.mode, Mode::Streaming { .. }));
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
fn tool_start_flips_mode_to_tool_running() {
    let mut app = App::new();
    app.on_turn_started();
    app.on_content_chunk("partial");
    app.on_tool_start(1, "grep".into(), "pattern=foo".into());
    match &app.mode {
        Mode::ToolRunning { tool, .. } => assert_eq!(tool, "grep"),
        other => panic!(
            "expected ToolRunning, got {:?}",
            std::mem::discriminant(other)
        ),
    }
}

#[test]
fn slash_finished_resets_mode_and_clears_cancel_tx() {
    // Regression: /model (and other slash commands) used to leave
    // the UI stuck in Mode::Thinking forever because the driver
    // only emitted SystemNote and never signalled completion.
    let mut app = App::new();
    let (tx, _rx) = tokio::sync::oneshot::channel();
    app.set_cancel_sender(tx);
    app.mode = Mode::Thinking {
        since: Instant::now(),
    };
    app.on_system_note("current model: foo".into());
    assert!(matches!(app.mode, Mode::Thinking { .. }));
    app.on_slash_finished();
    assert!(matches!(app.mode, Mode::Idle));
    assert!(app.cancel_tx.is_none());
}

#[test]
fn tool_done_returns_to_thinking_when_no_tools_pending() {
    let mut app = App::new();
    app.on_turn_started();
    app.on_tool_start(1, "grep".into(), "".into());
    app.on_tool_done(
        1,
        Duration::from_millis(500),
        "ok".into(),
        true,
        String::new(),
    );
    assert!(matches!(app.mode, Mode::Thinking { .. }));
}

#[test]
fn tool_done_stays_in_tool_running_when_other_tools_pending() {
    let mut app = App::new();
    app.on_turn_started();
    app.on_tool_start(1, "grep".into(), "".into());
    app.on_tool_start(2, "read".into(), "".into());
    app.on_tool_done(
        1,
        Duration::from_millis(500),
        "ok".into(),
        true,
        String::new(),
    );
    // Tool 2 is still running; mode should remain ToolRunning.
    assert!(matches!(app.mode, Mode::ToolRunning { .. }));
}

#[test]
fn content_chunk_after_tool_done_flips_to_streaming() {
    let mut app = App::new();
    app.on_turn_started();
    app.on_tool_start(1, "grep".into(), "".into());
    app.on_tool_done(
        1,
        Duration::from_millis(1),
        "ok".into(),
        true,
        String::new(),
    );
    assert!(matches!(app.mode, Mode::Thinking { .. }));
    app.on_content_chunk("here's what I found");
    assert!(matches!(app.mode, Mode::Streaming { .. }));
}

#[test]
fn tool_args_chunk_pushes_streaming_card_on_first_chunk() {
    // Phase Y2: while the model is streaming a tool call (before
    // PreToolUse dispatches it), we surface a streaming card carrying
    // the accumulated JSON so the renderer can show a diff peek.
    let mut app = App::new();
    app.on_turn_started();
    app.on_content_chunk("I'll edit foo.rs");
    app.on_tool_args_chunk("toolu_1".into(), "Edit".into(), "{\"file_path".into());
    match app.transcript.last().unwrap() {
        TranscriptItem::ToolCard { tool, state, .. } => {
            assert_eq!(tool, "Edit");
            match state {
                ToolCardState::Streaming {
                    provider_tool_id,
                    accumulated_json,
                } => {
                    assert_eq!(provider_tool_id, "toolu_1");
                    assert_eq!(accumulated_json, "{\"file_path");
                }
                _ => panic!("expected Streaming state"),
            }
        }
        _ => panic!("expected a ToolCard, got {:?}", app.transcript.last()),
    }
    // The active assistant item should be sealed off when streaming
    // tool-args begins (same as on_tool_start), so the next content
    // chunk starts a fresh Assistant item.
    assert!(app.active_assistant.is_none());
}

#[test]
fn tool_args_chunk_updates_accumulated_json_on_subsequent_chunks() {
    let mut app = App::new();
    app.on_turn_started();
    app.on_tool_args_chunk("toolu_1".into(), "Edit".into(), "{\"file_path".into());
    app.on_tool_args_chunk(
        "toolu_1".into(),
        "Edit".into(),
        "{\"file_path\":\"src/foo.rs\"".into(),
    );
    // One card, with the updated accumulated_json — no duplicate push.
    let cards: Vec<_> = app
        .transcript
        .iter()
        .filter(|i| matches!(i, TranscriptItem::ToolCard { .. }))
        .collect();
    assert_eq!(cards.len(), 1);
    match cards[0] {
        TranscriptItem::ToolCard {
            state: ToolCardState::Streaming {
                accumulated_json, ..
            },
            ..
        } => {
            assert_eq!(accumulated_json, "{\"file_path\":\"src/foo.rs\"");
        }
        _ => panic!(),
    }
}

#[test]
fn tool_args_chunk_separate_provider_ids_create_separate_cards() {
    // The model can compose multiple tool calls in a single turn; each
    // streams under its own provider_tool_id and gets its own card.
    let mut app = App::new();
    app.on_turn_started();
    app.on_tool_args_chunk("toolu_1".into(), "Edit".into(), "{".into());
    app.on_tool_args_chunk("toolu_2".into(), "Write".into(), "{".into());
    let cards: Vec<_> = app
        .transcript
        .iter()
        .filter(|i| matches!(i, TranscriptItem::ToolCard { .. }))
        .collect();
    assert_eq!(cards.len(), 2);
}

#[test]
fn tool_start_upgrades_matching_streaming_card_to_running() {
    // After streaming completes, the agent dispatches the tool and
    // ToolStart fires. The matching Streaming card should upgrade
    // in-place — same transcript index, state flips to Running, the
    // active_tools map is registered so the eventual ToolDone finds
    // the right slot.
    let mut app = App::new();
    app.on_turn_started();
    app.on_tool_args_chunk("toolu_1".into(), "Edit".into(), "{\"file_path".into());
    let streaming_idx = app.transcript.len() - 1;
    let len_before = app.transcript.len();

    app.on_tool_start(7, "Edit".into(), "file_path=src/foo.rs".into());

    assert_eq!(app.transcript.len(), len_before, "no duplicate push");
    match &app.transcript[streaming_idx] {
        TranscriptItem::ToolCard { tool, state, .. } => {
            assert_eq!(tool, "Edit");
            assert!(
                matches!(state, ToolCardState::Running { .. }),
                "state should have flipped to Running",
            );
        }
        _ => panic!("expected ToolCard at slot {}", streaming_idx),
    }
    // ToolDone for id=7 must close this exact card.
    app.on_tool_done(
        7,
        Duration::from_millis(1),
        "ok".into(),
        true,
        String::new(),
    );
    match &app.transcript[streaming_idx] {
        TranscriptItem::ToolCard { state, .. } => {
            assert!(matches!(state, ToolCardState::Done { .. }));
        }
        _ => panic!(),
    }
}

#[test]
fn tool_start_with_no_streaming_card_still_pushes_running_card() {
    // Providers that don't emit ToolArgsChunk (or models that compose
    // tool calls atomically) still work — no Streaming card means the
    // existing push-new-Running-card path runs unchanged.
    let mut app = App::new();
    app.on_turn_started();
    app.on_tool_start(1, "Read".into(), "file_path=x".into());
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
    app.on_tool_done(
        1,
        Duration::from_millis(1),
        "1 line".into(),
        true,
        String::new(),
    );
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
fn jump_to_prev_turn_moves_offset_to_last_turn_above_current() {
    let mut app = App::new();
    app.note_scroll_metrics(100, 10);
    app.turn_line_indices = vec![5, 25, 60, 90];
    app.scroll_manual = Some(70);
    app.jump_to_prev_turn();
    assert_eq!(app.scroll_manual, Some(60));
    app.jump_to_prev_turn();
    assert_eq!(app.scroll_manual, Some(25));
    app.jump_to_prev_turn();
    assert_eq!(app.scroll_manual, Some(5));
    // No turn above 5 → no-op.
    app.jump_to_prev_turn();
    assert_eq!(app.scroll_manual, Some(5));
}

#[test]
fn jump_to_prev_turn_when_attached_uses_scroll_max_as_current() {
    // Attached (None) treats the natural bottom as "current".
    let mut app = App::new();
    app.note_scroll_metrics(100, 10);
    app.turn_line_indices = vec![5, 25, 60, 90];
    app.scroll_manual = None;
    app.jump_to_prev_turn();
    assert_eq!(app.scroll_manual, Some(90));
}

#[test]
fn jump_to_next_turn_advances_or_reattaches_when_past_max() {
    let mut app = App::new();
    app.note_scroll_metrics(100, 10);
    app.turn_line_indices = vec![5, 25, 60, 90, 120];
    app.scroll_manual = Some(10);
    app.jump_to_next_turn();
    assert_eq!(app.scroll_manual, Some(25));
    app.jump_to_next_turn();
    assert_eq!(app.scroll_manual, Some(60));
    app.jump_to_next_turn();
    assert_eq!(app.scroll_manual, Some(90));
    // Next target is 120 which exceeds max=100 → reattach.
    app.jump_to_next_turn();
    assert_eq!(app.scroll_manual, None);
}

#[test]
fn jump_keys_are_ignored_while_input_has_text() {
    let mut app = App::new();
    app.note_scroll_metrics(100, 10);
    app.turn_line_indices = vec![5, 50];
    app.scroll_manual = Some(70);
    // Type a character first so `[` should land in the buffer
    // instead of triggering the jump.
    type_str(&mut app, "x");
    app.on_key(key(KeyCode::Char('[')));
    assert_eq!(input_string(&app), "x[");
    assert_eq!(app.scroll_manual, Some(70));
}

#[test]
fn jump_keys_fire_when_input_is_empty() {
    let mut app = App::new();
    app.note_scroll_metrics(100, 10);
    app.turn_line_indices = vec![5, 50];
    app.scroll_manual = Some(70);
    app.on_key(key(KeyCode::Char('[')));
    assert_eq!(app.scroll_manual, Some(50));
    // `[` should NOT have landed in the input buffer.
    assert_eq!(input_string(&app), "");
}

#[test]
fn position_stack_records_on_turn_jump_and_supports_back_forward() {
    let mut app = App::new();
    app.note_scroll_metrics(100, 10);
    app.turn_line_indices = vec![5, 25, 60, 90];
    app.scroll_manual = Some(95);
    // Two jumps back through turns; each records the prior pos.
    app.jump_to_prev_turn();
    assert_eq!(app.scroll_manual, Some(90));
    app.jump_to_prev_turn();
    assert_eq!(app.scroll_manual, Some(60));
    // Ctrl+O steps back. First Ctrl+O captures the current
    // position so Ctrl+I can return to it.
    app.jump_back_in_history();
    assert_eq!(app.scroll_manual, Some(90));
    app.jump_back_in_history();
    assert_eq!(app.scroll_manual, Some(95));
    // Ctrl+I steps forward back to where we started Ctrl+O-ing.
    app.jump_forward_in_history();
    assert_eq!(app.scroll_manual, Some(90));
    app.jump_forward_in_history();
    assert_eq!(app.scroll_manual, Some(60));
}

#[test]
fn position_stack_caps_at_history_limit() {
    let mut app = App::new();
    app.note_scroll_metrics(1000, 10);
    for i in 0..(SCROLL_HISTORY_CAP + 5) {
        app.scroll_manual = Some((i * 10) as u16);
        app.record_scroll_position();
    }
    assert!(app.scroll_positions.len() <= SCROLL_HISTORY_CAP);
}

#[test]
fn position_stack_truncates_forward_history_on_new_record() {
    let mut app = App::new();
    app.note_scroll_metrics(100, 10);
    app.turn_line_indices = vec![10, 30, 60, 90];
    app.scroll_manual = Some(95);
    app.jump_to_prev_turn(); // records 95, lands at 90
    app.jump_to_prev_turn(); // records 90, lands at 60
    app.jump_back_in_history(); // back to 90 (cursor mid-stack)
    let stack_len_before = app.scroll_positions.len();
    // A new recording while cursor is mid-stack drops everything
    // past the cursor.
    app.scroll_manual = Some(15);
    app.record_scroll_position();
    assert!(app.scroll_positions.len() <= stack_len_before);
    // Forward jump is now a no-op (no entries past the cursor).
    let before = app.scroll_manual;
    app.jump_forward_in_history();
    assert_eq!(app.scroll_manual, before);
}

#[test]
fn ctrl_o_back_jump_fires_from_key_handler_when_input_empty() {
    let mut app = App::new();
    app.note_scroll_metrics(100, 10);
    app.turn_line_indices = vec![10, 50];
    app.scroll_manual = Some(80);
    app.on_key(key(KeyCode::Char('['))); // jump to prev turn (50)
    assert_eq!(app.scroll_manual, Some(50));
    app.on_key(ctrl('o'));
    assert_eq!(app.scroll_manual, Some(80));
    app.on_key(ctrl('i'));
    assert_eq!(app.scroll_manual, Some(50));
}

#[test]
fn help_browser_opens_with_slash_meta_sorted() {
    let mut app = App::new();
    app.set_slash_meta(vec![
        ("model".into(), "swap models".into()),
        ("clear".into(), "clear memory".into()),
    ]);
    app.open_help_browser();
    let b = app.help_browser().expect("browser open");
    let names: Vec<&str> = b.entries.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, vec!["clear", "model"]);
    assert_eq!(b.selected, 0);
}

#[test]
fn help_browser_navigate_wraps_around() {
    let mut app = App::new();
    app.set_slash_meta(vec![
        ("a".into(), "".into()),
        ("b".into(), "".into()),
        ("c".into(), "".into()),
    ]);
    app.open_help_browser();
    app.help_browser_navigate(-1);
    assert_eq!(app.help_browser().unwrap().selected, 2);
    app.help_browser_navigate(1);
    assert_eq!(app.help_browser().unwrap().selected, 0);
}

#[test]
fn sessions_picker_pick_returns_selected_id() {
    let mut app = App::new();
    app.open_sessions_picker(vec![
        SessionPickerRow {
            id: "111".into(),
            label: "111 (3m ago)".into(),
        },
        SessionPickerRow {
            id: "222".into(),
            label: "222 (1h ago)".into(),
        },
    ]);
    app.sessions_picker_navigate(1);
    assert_eq!(app.sessions_picker_pick(), Some("222".into()));
}

#[test]
fn inline_help_pulls_description_from_slash_meta() {
    let mut app = App::new();
    app.set_slash_meta(vec![("cost".into(), "show token usage".into())]);
    app.open_inline_help("cost");
    let card = app.inline_help().expect("card open");
    assert_eq!(card.name, "cost");
    assert_eq!(card.description, "show token usage");
}

#[test]
fn inline_help_falls_back_when_command_unknown() {
    let mut app = App::new();
    app.open_inline_help("frobnicate");
    let card = app.inline_help().unwrap();
    assert!(card.description.contains("no help registered"));
}

#[test]
fn undo_pops_the_last_user_prompt_and_returns_its_body() {
    let mut app = App::new();
    // Simulate a turn: user prompt + assistant response +
    // tool card. Undo should drop them all.
    app.transcript.push(TranscriptItem::UserPrompt {
        body: "first prompt".into(),
    });
    app.transcript.push(TranscriptItem::Assistant {
        body: "first response".into(),
        done: true,
    });
    app.on_turn_started();
    app.on_content_chunk("...");
    // Simulate a tool round mid-turn.
    app.on_tool_start(1, "Read".into(), "x".into());
    app.on_tool_done(
        1,
        Duration::from_millis(1),
        "1 line".into(),
        true,
        String::new(),
    );
    app.on_content_chunk("more");
    app.on_turn_finished("");

    // Now run undo: removes the most recent UserPrompt and
    // every transcript item after it.
    let popped = app.undo_last_user_turn();
    // Hmm — `on_turn_started` doesn't push a UserPrompt; only
    // submit() does. We pushed one manually for the first
    // turn though, so undo finds it.
    assert_eq!(popped.as_deref(), Some("first prompt"));
    // Every transcript item after the user prompt is gone.
    let any_assistant = app
        .transcript
        .iter()
        .any(|i| matches!(i, TranscriptItem::Assistant { .. }));
    assert!(!any_assistant, "transcript should be trimmed");
    assert!(app.active_assistant.is_none());
    assert!(app.active_tools.is_empty());
}

#[test]
fn undo_returns_none_when_no_user_prompt_in_transcript() {
    let mut app = App::new();
    // Default new-app transcript is just a System welcome
    // note.
    assert!(app.undo_last_user_turn().is_none());
}

#[test]
fn set_input_text_pub_replaces_buffer_for_edit_and_rerun() {
    let mut app = App::new();
    type_str(&mut app, "draft");
    app.set_input_text_pub("re-edit me");
    assert_eq!(input_string(&app), "re-edit me");
}

#[test]
fn history_search_returns_newest_first_when_query_is_empty() {
    let mut app = App::new();
    app.set_history(vec!["one".into(), "two".into(), "three".into()]);
    app.open_history_search();
    let s = app.history_search().unwrap();
    // newest-first: indices 2, 1, 0
    assert_eq!(s.matches, vec![2, 1, 0]);
    assert_eq!(s.selected, 0);
}

#[test]
fn history_search_filters_by_substring_case_insensitive() {
    let mut app = App::new();
    app.set_history(vec![
        "edit src/main.rs".into(),
        "show CARGO.toml".into(),
        "list files".into(),
    ]);
    app.open_history_search();
    for c in "Cargo".chars() {
        app.history_search_push_char(c);
    }
    let s = app.history_search().unwrap();
    // Only the second entry matches `cargo`. Index 1.
    assert_eq!(s.matches, vec![1]);
}

#[test]
fn history_search_navigate_wraps() {
    let mut app = App::new();
    app.set_history(vec!["a".into(), "b".into(), "c".into()]);
    app.open_history_search();
    app.history_search_navigate(-1);
    assert_eq!(app.history_search().unwrap().selected, 2);
    app.history_search_navigate(1);
    assert_eq!(app.history_search().unwrap().selected, 0);
}

#[test]
fn history_search_pick_returns_body_for_selected_match() {
    let mut app = App::new();
    app.set_history(vec!["alpha".into(), "beta".into(), "gamma".into()]);
    app.open_history_search();
    app.history_search_navigate(1); // selects newest-1 == "beta"
    assert_eq!(app.history_search_pick(), Some("beta".into()));
}

#[test]
fn history_search_backspace_re_widens_results() {
    let mut app = App::new();
    app.set_history(vec!["alpha".into(), "alphabet".into(), "beta".into()]);
    app.open_history_search();
    app.history_search_push_char('a');
    app.history_search_push_char('l');
    app.history_search_push_char('p');
    // Two matches: alphabet (idx 1), alpha (idx 0).
    assert_eq!(app.history_search().unwrap().matches, vec![1, 0]);
    app.history_search_backspace();
    app.history_search_backspace();
    app.history_search_backspace();
    // Empty query → all entries newest-first.
    assert_eq!(app.history_search().unwrap().matches, vec![2, 1, 0]);
}

#[test]
fn hint_seen_state_round_trips_through_mark() {
    let mut app = App::new();
    assert!(app.hint_is_unseen("approval-allow"));
    app.mark_hint_shown("approval-allow");
    assert!(!app.hint_is_unseen("approval-allow"));
}

#[test]
fn wheel_down_reattaches_at_bottom() {
    let mut app = App::new();
    app.note_scroll_metrics(30, 10);
    app.scroll_manual = Some(28);
    app.scroll_wheel_down(3);
    assert_eq!(app.scroll_manual, None);
}

#[test]
fn copy_fallback_opens_with_body_and_index() {
    let mut app = App::new();
    app.open_copy_fallback("hello world".into(), 1, "neovim:terminal".into());
    let s = app.copy_fallback().expect("overlay should be open");
    assert_eq!(s.body, "hello world");
    assert_eq!(s.index, 1);
    assert_eq!(s.host_hint, "neovim:terminal");
    assert_eq!(s.scroll, 0);
}

#[test]
fn copy_fallback_close_clears_overlay() {
    let mut app = App::new();
    app.open_copy_fallback("body".into(), 2, "vscode".into());
    assert!(app.copy_fallback().is_some());
    app.close_copy_fallback();
    assert!(app.copy_fallback().is_none());
}

// ---------- Phase Y4: card focus + expand ----------

fn push_done_card(app: &mut App, tool: &str, full_output: &str) {
    use crate::tui::app::transcript::ToolCardState;
    app.transcript.push(TranscriptItem::ToolCard {
        tool: tool.into(),
        args_preview: "".into(),
        state: ToolCardState::Done {
            duration: Duration::from_millis(1),
            summary: "ok".into(),
            ok: true,
            full_output: full_output.into(),
            expanded: false,
        },
    });
}

#[test]
fn focus_prev_card_lands_on_last_when_no_focus() {
    let mut app = App::new();
    app.transcript.clear();
    push_done_card(&mut app, "Read", "");
    push_done_card(&mut app, "Bash", "");
    push_done_card(&mut app, "Edit", "");
    app.focus_prev_card();
    assert_eq!(app.focused_card_idx, Some(2));
}

#[test]
fn focus_next_card_lands_on_first_when_no_focus() {
    let mut app = App::new();
    app.transcript.clear();
    push_done_card(&mut app, "Read", "");
    push_done_card(&mut app, "Bash", "");
    app.focus_next_card();
    assert_eq!(app.focused_card_idx, Some(0));
}

#[test]
fn focus_next_card_advances_through_cards_then_wraps() {
    let mut app = App::new();
    app.transcript.clear();
    push_done_card(&mut app, "A", "");
    push_done_card(&mut app, "B", "");
    push_done_card(&mut app, "C", "");
    app.focus_next_card();
    assert_eq!(app.focused_card_idx, Some(0));
    app.focus_next_card();
    assert_eq!(app.focused_card_idx, Some(1));
    app.focus_next_card();
    assert_eq!(app.focused_card_idx, Some(2));
    app.focus_next_card();
    assert_eq!(app.focused_card_idx, Some(0));
}

#[test]
fn focus_prev_card_walks_backward_then_wraps() {
    let mut app = App::new();
    app.transcript.clear();
    push_done_card(&mut app, "A", "");
    push_done_card(&mut app, "B", "");
    push_done_card(&mut app, "C", "");
    app.focus_prev_card();
    assert_eq!(app.focused_card_idx, Some(2));
    app.focus_prev_card();
    assert_eq!(app.focused_card_idx, Some(1));
    app.focus_prev_card();
    assert_eq!(app.focused_card_idx, Some(0));
    app.focus_prev_card();
    assert_eq!(app.focused_card_idx, Some(2));
}

#[test]
fn focus_nav_skips_non_done_cards_and_interleaves_with_other_items() {
    use crate::tui::app::transcript::ToolCardState;
    let mut app = App::new();
    app.transcript.clear();
    push_done_card(&mut app, "Read", "");
    app.transcript.push(TranscriptItem::Assistant {
        body: "thinking".into(),
        done: true,
    });
    // A Running card — should be skipped.
    app.transcript.push(TranscriptItem::ToolCard {
        tool: "Bash".into(),
        args_preview: "".into(),
        state: ToolCardState::Running {
            started_at: std::time::Instant::now(),
        },
    });
    push_done_card(&mut app, "Write", "");
    app.focus_next_card();
    assert_eq!(app.focused_card_idx, Some(0)); // Read
    app.focus_next_card();
    assert_eq!(app.focused_card_idx, Some(3)); // Write — skipped Running
}

#[test]
fn focus_nav_no_op_when_no_done_cards() {
    let mut app = App::new();
    app.transcript.clear();
    app.focus_next_card();
    assert_eq!(app.focused_card_idx, None);
    app.focus_prev_card();
    assert_eq!(app.focused_card_idx, None);
}

#[test]
fn toggle_focused_card_expanded_flips_state() {
    use crate::tui::app::transcript::ToolCardState;
    let mut app = App::new();
    app.transcript.clear();
    push_done_card(&mut app, "Read", "line1\nline2");
    app.focus_next_card();
    assert!(app.toggle_focused_card_expanded());
    let expanded_after_first = matches!(
        &app.transcript[0],
        TranscriptItem::ToolCard {
            state: ToolCardState::Done { expanded: true, .. },
            ..
        }
    );
    assert!(expanded_after_first);
    assert!(app.toggle_focused_card_expanded());
    let expanded_after_second = matches!(
        &app.transcript[0],
        TranscriptItem::ToolCard {
            state: ToolCardState::Done {
                expanded: false,
                ..
            },
            ..
        }
    );
    assert!(expanded_after_second);
}

#[test]
fn toggle_focused_card_expanded_no_op_without_focus() {
    let mut app = App::new();
    app.transcript.clear();
    push_done_card(&mut app, "Read", "x");
    assert!(!app.toggle_focused_card_expanded());
}

#[test]
fn clear_card_focus_drops_the_focus_pointer() {
    let mut app = App::new();
    app.transcript.clear();
    push_done_card(&mut app, "Read", "");
    app.focus_next_card();
    assert_eq!(app.focused_card_idx, Some(0));
    app.clear_card_focus();
    assert_eq!(app.focused_card_idx, None);
}

#[test]
fn copy_fallback_scroll_is_bounded_at_zero() {
    let mut app = App::new();
    app.open_copy_fallback("body".into(), 1, "host".into());
    app.copy_fallback_scroll_up();
    // PgUp at the top of a fresh modal stays at 0 — saturating_sub.
    assert_eq!(app.copy_fallback().unwrap().scroll, 0);
    app.copy_fallback_scroll_down();
    assert_eq!(app.copy_fallback().unwrap().scroll, 5);
    app.copy_fallback_scroll_up();
    assert_eq!(app.copy_fallback().unwrap().scroll, 0);
}

// ---------- inline commit watermark (committable_count) ----------

fn user(body: &str) -> TranscriptItem {
    TranscriptItem::UserPrompt {
        body: body.to_string(),
    }
}
fn assistant(body: &str, done: bool) -> TranscriptItem {
    TranscriptItem::Assistant {
        body: body.to_string(),
        done,
    }
}
fn sys(body: &str) -> TranscriptItem {
    TranscriptItem::System {
        body: body.to_string(),
    }
}
fn card_running() -> TranscriptItem {
    TranscriptItem::ToolCard {
        tool: "Read".into(),
        args_preview: "(README.md)".into(),
        state: ToolCardState::Running {
            started_at: Instant::now(),
        },
    }
}
fn card_done() -> TranscriptItem {
    TranscriptItem::ToolCard {
        tool: "Read".into(),
        args_preview: "(README.md)".into(),
        state: ToolCardState::Done {
            duration: Duration::from_millis(1),
            summary: "ok".into(),
            ok: true,
            full_output: String::new(),
            expanded: false,
        },
    }
}

#[test]
fn committable_count_empty_transcript_is_zero() {
    assert_eq!(committable_count(&[], 0), 0);
}

#[test]
fn committable_count_commits_all_when_every_item_is_final() {
    let items = vec![user("hi"), assistant("hello", true), sys("note")];
    assert_eq!(committable_count(&items, 0), 3);
}

#[test]
fn committable_count_stops_at_first_live_item() {
    // user(final) + assistant streaming(live) — only the prompt commits,
    // exactly the after-submit case that orphaned the idle hint.
    let items = vec![user("hi"), assistant("", false)];
    assert_eq!(committable_count(&items, 0), 1);
}

#[test]
fn committable_count_does_not_skip_past_a_live_item() {
    // A live tool card mid-turn blocks commit of the finished assistant
    // segment that follows it, preserving scrollback ordering.
    let items = vec![
        user("hi"),
        assistant("thinking", true),
        card_running(),
        assistant("after tool", true),
    ];
    assert_eq!(committable_count(&items, 0), 2);
}

#[test]
fn committable_count_advances_after_a_card_finishes() {
    let mut items = vec![user("hi"), assistant("seg", true), card_running()];
    assert_eq!(committable_count(&items, 0), 2);
    // Card finishes → the whole prefix becomes committable.
    items[2] = card_done();
    assert_eq!(committable_count(&items, 2), 3);
}

#[test]
fn committable_count_is_idempotent() {
    let items = vec![user("hi"), assistant("hello", true)];
    let n = committable_count(&items, 0);
    assert_eq!(n, 2);
    // Re-running from the returned watermark advances nothing.
    assert_eq!(committable_count(&items, n), 2);
}

#[test]
fn committable_count_clamps_committed_over_len() {
    // Defensive: a stale watermark past the end never panics.
    let items = vec![user("hi")];
    assert_eq!(committable_count(&items, 5), 1);
}

#[test]
fn undo_clamps_committed_watermark() {
    let mut app = App::new();
    app.transcript = vec![user("hi"), assistant("hello", true)];
    app.committed = 2; // both flushed to scrollback
    app.undo_last_user_turn();
    // Transcript truncated to before the user prompt; watermark must
    // not dangle past the new length.
    assert_eq!(app.transcript.len(), 0);
    assert_eq!(app.committed, 0);
}

#[test]
fn approval_selection_clamps_at_both_ends() {
    use crate::tui::app::overlay::APPROVAL_OPTIONS;
    let mut app = App::new();
    app.on_approval_requested(
        "Bash".into(),
        serde_json::json!({"command":"ls"}),
        "run".into(),
    );
    app.approval_select_prev();
    assert_eq!(app.approval().unwrap().selected, 0);
    for _ in 0..10 {
        app.approval_select_next();
    }
    assert_eq!(app.approval().unwrap().selected, APPROVAL_OPTIONS.len() - 1);
}

#[test]
fn approval_response_for_maps_list_order() {
    use crate::tui::event::{ApprovalResponse, approval_response_for};
    assert!(matches!(approval_response_for(0), ApprovalResponse::Yes));
    assert!(matches!(approval_response_for(1), ApprovalResponse::No));
    assert!(matches!(
        approval_response_for(2),
        ApprovalResponse::AlwaysAllow
    ));
    assert!(matches!(
        approval_response_for(3),
        ApprovalResponse::PersistAllow
    ));
    assert!(matches!(
        approval_response_for(4),
        ApprovalResponse::AlwaysDeny
    ));
}

#[test]
fn decision_cell_lands_in_transcript_with_glyph() {
    use crate::tui::event::ApprovalResponse;
    let mut app = App::new();
    app.on_approval_requested(
        "Bash".into(),
        serde_json::json!({"command":"ls"}),
        "run".into(),
    );
    app.note_approval_decision(&ApprovalResponse::Yes);
    let last = app.transcript.last().unwrap();
    match last {
        TranscriptItem::System { body } => {
            assert!(body.starts_with("✔ "), "got: {body}");
            assert!(body.contains("Bash"), "got: {body}");
        }
        other => panic!("expected System, got {other:?}"),
    }
    app.note_approval_decision(&ApprovalResponse::AlwaysDeny);
    match app.transcript.last().unwrap() {
        TranscriptItem::System { body } => assert!(body.starts_with("✗ "), "got: {body}"),
        _ => panic!(),
    }
}

#[test]
fn welcome_and_turn_rule_are_immediately_committable() {
    let items = vec![
        TranscriptItem::Welcome,
        TranscriptItem::TurnRule {
            elapsed: Duration::ZERO,
        },
    ];
    assert_eq!(committable_count(&items, 0), 2);
}
