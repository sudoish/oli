//! Tests for the `app` module — extracted so `mod.rs` stays
//! focused on production code. Brought in via
//! `#[cfg(test)] mod tests;` from mod.rs, so the file's
//! contents form the body of that module and have full access
//! to private items via `use super::*;`.

use super::*;
use crossterm::event::KeyCode;
use std::time::Duration;

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
    assert_eq!(menu.candidates, vec!["clear".to_string(), "cost".to_string()]);
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
        other => panic!("expected ToolRunning, got {:?}", std::mem::discriminant(other)),
    }
}

#[test]
fn tool_done_returns_to_thinking_when_no_tools_pending() {
    let mut app = App::new();
    app.on_turn_started();
    app.on_tool_start(1, "grep".into(), "".into());
    app.on_tool_done(1, Duration::from_millis(500), "ok".into(), true);
    assert!(matches!(app.mode, Mode::Thinking { .. }));
}

#[test]
fn tool_done_stays_in_tool_running_when_other_tools_pending() {
    let mut app = App::new();
    app.on_turn_started();
    app.on_tool_start(1, "grep".into(), "".into());
    app.on_tool_start(2, "read".into(), "".into());
    app.on_tool_done(1, Duration::from_millis(500), "ok".into(), true);
    // Tool 2 is still running; mode should remain ToolRunning.
    assert!(matches!(app.mode, Mode::ToolRunning { .. }));
}

#[test]
fn content_chunk_after_tool_done_flips_to_streaming() {
    let mut app = App::new();
    app.on_turn_started();
    app.on_tool_start(1, "grep".into(), "".into());
    app.on_tool_done(1, Duration::from_millis(1), "ok".into(), true);
    assert!(matches!(app.mode, Mode::Thinking { .. }));
    app.on_content_chunk("here's what I found");
    assert!(matches!(app.mode, Mode::Streaming { .. }));
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
    app.transcript
        .push(TranscriptItem::UserPrompt { body: "first prompt".into() });
    app.transcript.push(TranscriptItem::Assistant {
        body: "first response".into(),
        done: true,
    });
    app.on_turn_started();
    app.on_content_chunk("...");
    // Simulate a tool round mid-turn.
    app.on_tool_start(1, "Read".into(), "x".into());
    app.on_tool_done(1, Duration::from_millis(1), "1 line".into(), true);
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
