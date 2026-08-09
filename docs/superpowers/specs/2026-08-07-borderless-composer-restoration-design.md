# Borderless composer restoration design

## Goal

Restore oli's previous Codex-style borderless composer without weakening the
fixed-layout and redraw safeguards that eliminated stale progress-row shadows.

## Design

Keep the composer allocated as a fixed five-row region. Paint the entire region
with `theme.user_band_bg`, render a `›` prompt in a three-column left gutter,
and render the existing multiline `tui_textarea::TextArea` in the remaining
inner rectangle with one blank row above and below it.

While the agent is busy, retain the same full-region tint and prompt alignment,
but replace editable text with the existing cancellation hint. Do not restore
dynamic composer height, borders, transcript bottom anchoring, or any layout
change based on input contents.

The full-width progress band, diagnostic stderr suspension, and one-shot
terminal invalidation on approval and progress-mode transitions remain
unchanged.

## Testing

Use Ratatui's `TestBackend` to assert that the composer has no border glyphs,
the tint covers every cell, the second row begins with ` › ` followed by input
text, and multiline content remains clipped inside the fixed five-row region.
Run focused UI tests and the complete library suite.
