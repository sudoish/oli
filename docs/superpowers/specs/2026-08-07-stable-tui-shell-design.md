# Stable TUI shell design

## Goal

Eliminate streamed transcript content appearing in the progress or composer
region while keeping oli's TUI entirely in Rust. Establish a deliberately
plain Ratatui shell before restoring optional presentation features.

## Scope

Keep the existing agent driver, event channel, transcript model, markdown
renderer, tool cards, overlays, footer data, and fullscreen/inline terminal
lifecycle. Change only the ordinary conversation shell and its scroll math.

The shell has four non-overlapping vertical regions:

1. A flexible transcript `Paragraph`.
2. A fixed one-row progress `Paragraph`.
3. A fixed three-row bordered composer using the existing `tui-textarea`
   input state.
4. A fixed one-row footer `Paragraph`.

The transcript starts at the top of its region and uses ordinary bottom-scroll
offsets when its rendered content exceeds the available height. It does not
prepend synthetic blank lines to anchor short content to the bottom. The
composer does not grow with its contents; input remains multiline and scrolls
inside its fixed region.

The progress region displays only the activity spinner, state, elapsed time,
and cancellation hint. Tool details continue to appear in transcript cards.

## Rendering invariants

- Each widget receives exactly its own `Rect`; transcript rendering never
  targets the progress, composer, or footer rows.
- Region heights do not change when the first streaming chunk arrives or while
  tools start and finish.
- Every ordinary-shell region paints its full rectangle on every frame so stale
  cells are cleared through Ratatui's normal buffer diff.
- Overlays retain their current dedicated rendering paths.
- Inline transcript commits remain unchanged; this work must also behave
  correctly in fullscreen mode, where the reported bug reproduces.

## Testing

Add a `TestBackend` regression that renders an initial busy frame and then a
streaming-content frame into the same terminal. Assert that transcript markers
exist only inside the transcript rectangle and that the progress, composer,
and footer contents remain in their assigned rows after the second draw.

Add focused layout tests for fixed region heights and a composer test proving
that multiline input remains contained within its bordered rectangle. Run the
focused TUI tests followed by `cargo test --lib`.

## Explicit non-goals

- Rewriting the TUI in Go or replacing Ratatui.
- Changing agent streaming, provider behavior, or tool execution.
- Redesigning overlays, completion, search, approvals, or transcript cards.
- Restoring a dynamically growing composer or bottom-anchored short transcript
  in the same change. Those can return individually after the stable baseline
  is manually verified.
