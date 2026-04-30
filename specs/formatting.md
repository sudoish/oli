# Pretty Output — Improving Agent Response Formatting

A focused polish pass on how the harness renders model output and tool
results. Everything in `tui.md` phases A–T shipped a deterministic TUI —
this doc fills the gaps in markdown coverage and ANSI hygiene so the
assistant output *stays* readable when the model emits tables, code with
ANSI escapes, or HTML fragments.

## Current state (after Phase T)

The TUI has a working markdown → ratatui `Vec<Line>` renderer
(`src/tui/markdown.rs`). It handles:

- Headings with colored `▌` gutters + bold
- Bold, italic, strikethrough
- Inline code with `Modifier::REVERSED`
- Fenced code blocks with syntect syntax highlighting
- Lists (ordered `1.`, unordered `•`)
- Links: `text` underlined + dim `(url)` parens
- Horizontal rules

The `--plain` REPL (`src/repl/mod.rs`) has **zero formatting**: raw
markdown streamed to stdout.

**Definitive gaps** (verified by reading code + existing tests):

1. **ANSI escape codes contaminate tool results.** `Bash` captures raw
   stdout/stderr. Commands like `git log --color=always`, `cargo test --
   --color=always`, or `cat file_with_ansi` produce `\x1b[31m…\x1b[0m`
   sequences that are stored verbatim into agent memory and displayed
   literally in both TUI tool cards and model context. This breaks every
   downstream rendering layer immediately.

2. **Markdown tables are parsed but silently dropped.** `pulldown-cmark`
   parses GFM tables, but `markdown.rs` has no `Tag::Table*` handlers.
   The table body is emitted as a jumble of `|` and `---` with no layout
   or alignment, which looks like garbage in the TUI.

3. **HTML tags cause content loss.** `Event::Html` / `Event::InlineHtml`
   hit an empty branch comment that says "Treat as literal text" but
   actually does nothing. Content inside `<details>`, `<summary>`, etc.
   disappears entirely instead of being rendered as raw text.

4. **Code fence lines wrap without gutter continuation.** Ratatui's
   `Wrap { trim: false }` wraps long code lines, but the `│ ` gutter only
   appears on the first wrapped segment. Subsequent wrapped lines start
   at column 0, visually breaking the code-fence box.

5. **Inline code uses `Modifier::REVERSED` — ugly on many terminals.**
   Inverse-video looks harsh and is unreadable on some color schemes.

6. **Blockquotes are plain text with no visual distinction.** `>` quoted
   text renders identically to regular paragraphs.

7. **`--plain` REPL has no formatting at all.** Users on `oli --plain`
   see raw markdown streamed directly to stdout.

## Non-goals

- Rewriting the whole TUI rendering pipeline. We keep the existing
  `pulldown-cmark` → `Vec<Line>` architecture.
- Adding a new markdown parser. `pulldown-cmark` is correct and fast.
- Full HTML rendering. We render HTML as raw text, not parsed DOM.
- Custom theming beyond light/dark detection that already exists.
- Mouse interaction changes.

## Approach

Six independent steps, each gated by tests and each exactly one PR-sized
commit. The ordering is Phase 1 → Phase 2 → Phase 3 because later
markdown changes may conflict; Phase 4 and Phase 5 are independent and
can land in any order.

---

### Phase U1 — Strip ANSI escapes from tool output (highest impact)

**Problem:** Agent memory and tool cards contain literal escape codes.
**Solution:** Strip ANSI sequences at the source before text enters the
agent loop.

#### Files

- `src/tools/util.rs` — add `strip_ansi_codes(s: &str) -> Cow<'_, str>`.
  Small hand-rolled FSM: scan for `\x1b`, then `[…m`, `(…B`, `]8;…\\`
  (OSC hyperlink), or `K`/`J` sequence. Return `Cow::Borrowed(s)` when
  no codes are found (fast path).
- `src/tools/bash.rs` — call `strip_ansi_codes` in `format_output`
  before assembling the result string.
- `src/tools/grep.rs` — wrap ripgrep output through `strip_ansi_codes`
  when `--color=always` leaks through.
- `src/mcp/tool.rs` — strip ANSI from each text block in
  `format_tool_result`.
- `src/tools/read.rs` — optionally strip ANSI for detected content if
  `--color=always` was recorded in the file itself (rare, but free).

#### Acceptance

```rust
#[test]
fn strip_ansi_cleans_git_log_output() {
    let raw = "\x1b[33mcommit abc\x1b[0m hello";
    assert_eq!(strip_ansi_codes(raw), "commit abc hello");
}

#[test]
fn strip_ansi_is_identity_when_clean() {
    let s = "plain text no codes";
    match strip_ansi_codes(s) {
        std::borrow::Cow::Borrowed(_) => {}
        _ => panic!("should borrow, not allocate"),
    }
}
```

Also: `Bash` tool test that runs `printf '\e[31mred\e[0m'` and verifies
output is `"red"`.

#### Done when

- `cargo test` passes including new tests.
- `Bash` output of `printf '\e[31mhello\e[0m'` shows `hello` in tool
  cards, no escape codes.
- No new dependencies (hand-rolled FSM is <30 LOC).

---

### Phase U2 — Render markdown tables as box-drawing grids

**Problem:** `pulldown-cmark` parses tables, but `markdown.rs` has zero
`Table*` handlers, so table content is a scrambled mess.
**Solution:** Accumulate table cells during parsing, render at
`TagEnd::Table` as a `│───│` grid.

#### Files

- `src/tui/markdown.rs`:
  - Enable `Options::ENABLE_TABLES` in `Parser::new_ext`.
  - Add `in_table: bool` flag to `Renderer`.
  - Add `table_rows: Vec<Vec<Vec<Span<'static>>>>` accumulator.
  - Add `table_alignments: Vec<Alignment>`.
  - Handle `Tag::Table(alignment)` — set `in_table = true`.
  - Handle `Tag::TableHead` — push a new row vector.
  - Handle `Tag::TableRow` — push a new row vector.
  - Handle `Tag::TableCell` — push current `cur` spans into row.
  - On `TagEnd::Table` — compute column widths (max grapheme count
    across all cells in column), render top border, each row with
    `│ cell │` separators, alignment-aware padding.

#### Rendering rules

- Top border: `  ┌───┬───┐` (using Unicode box chars, same family as
  code-fence borders).
- Cell separator: `│` with leading `  ` gutter just like code blocks.
- Row separator between header and body: `  ├───┼───┤`.
- Bottom border: `  └───┴───┘`.
- Alignment: `Alignment::Left` pads right, `Center` centers, `Right` pads
  left. Default to left.
- Cells may contain inline styles (bold, code, etc.) because `cur` spans
  are captured as-is.
- If the table is wider than the viewport `area.width - 4`, clip cells
  to `max_cell_width` and append `…`. The rendering function needs a
  `max_width: u16` parameter.

#### Changes to `markdown::render` signature

```rust
// old
pub fn render(body: &str, theme: Theme) -> Vec<Line<'static>>;

// new
pub fn render(body: &str, theme: Theme, wrap_width: Option<u16>) -> Vec<Line<'static>>;
```

Call sites in `src/tui/ui/transcript.rs` pass `Some(area.width)`.
Tests pass `Some(120)`.

#### Acceptance

```rust
#[test]
fn simple_table_renders_with_grid() {
    let body = "| a | b |\n|---|---|\n| 1 | 2 |\n";
    let lines = render(body, Theme::Dark, Some(80));
    let txt = lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()).collect::<Vec<_>>().join("\n");
    assert!(txt.contains("┌"), "missing top border: {}", txt);
    assert!(txt.contains("│ a │ b │"), "missing cells: {}", txt);
    assert!(txt.contains("├"), "missing mid border: {}", txt);
    assert!(txt.contains("└"), "missing bottom border: {}", txt);
}
```

#### Done when

- Table renders as bordered grid in TUI.
- Inline styles inside table cells are preserved.
- Overflow cells are clipped with `…`.
- No panic on empty table or single-column table.

---

### Phase U3 — HTML passthrough + blockquote support

**Problem:** HTML tags silently drop content; blockquotes look like
plain text.
**Solution:** Render HTML as raw text; add blockquote `│ ` gutter.

#### Files

- `src/tui/markdown.rs`:
  - `Event::Html(h)` / `Event::InlineHtml(h)` → `self.push_text(&h)`
    instead of doing nothing.
  - Handle `Tag::BlockQuote` — set `in_blockquote = true`, push a
    `BlockQuoteState` to a stack.
  - On `TagEnd::BlockQuote` — pop stack.
  - Inside blockquote, prepend a dim `│ ` span to each line as it's
    flushed.
  - Nested blockquotes prepend additional `│ ` per nesting depth
    (e.g. `│ │ ` for depth 2).
  - Style: `Color::DarkGray`, no extra modifiers.

#### Rendering rules

- Blockquote flush: when a line is flushed inside a blockquote, the
  leading spans become:
  ```
  Span::styled("  │ ", Style::default().fg(Color::DarkGray))
  ```
  followed by the normal `cur` spans.
- Nested depth = stack height at flush time.
- Blank lines inside blockquotes still get the `│ ` prefix (so the quote
  gutter is continuous).

#### Acceptance

```rust
#[test]
fn html_renders_as_literal_text() {
    let lines = render("<details>hidden</details>", Theme::Dark, Some(80));
    let txt = lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()).collect::<Vec<_>>().join("\n");
    assert!(txt.contains("<details>"), "html dropped: {}", txt);
    assert!(txt.contains("hidden"), "html content dropped: {}", txt);
}

#[test]
fn blockquote_has_dim_gutter() {
    let lines = render("> hello", Theme::Dark, Some(80));
    let line = lines.iter().find(|l| {
        let s: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
        s.contains("hello")
    }).unwrap();
    let gutter: String = line.spans.iter().take_while(|s| !s.content.contains("hello")).map(|s| s.content.as_ref()).collect();
    assert!(gutter.contains("│"), "missing quote gutter: {:?}", line);
}
```

#### Done when

- `<details>text</details>` renders with tags and content visible.
- `> quoted` shows dim `│ ` gutter.
- `> > nested` shows double gutter `│ │ `.
- No regression in existing markdown tests.

---

### Phase U4 — Code fence wrapping with gutter continuation

**Problem:** Ratatui `Wrap` breaks long code lines without the `│ `
gutter on wrapped continuation lines.
**Solution:** Manual line wrapping inside `flush_code_block`.

#### Files

- `src/tui/markdown.rs`:
  - `flush_code_block` takes `wrap_width: u16`.
  - For each code line, if its display width (grapheme count) exceeds
    `wrap_width.saturating_sub(4)` (room for `│ ` prefix + right pad),
    split into chunks.
  - Each chunk gets its own `│ ` gutter at front.
  - Use `unicode_width::UnicodeWidthStr` for grapheme-aware width
    calculation (already a transitive dep; add explicit dep if needed).

#### Changes to data flow

The `render` function already gains `wrap_width: Option<u16>` in Phase
U2. In `flush_code_block`, unwrap the width or default to a large value
(e.g. 200) for the plain fallback.

#### Line-wrapping algorithm (simple)

```rust
fn wrap_line(line: &str, max_chars: usize) -> Vec<&str> {
    if line.chars().count() <= max_chars {
        return vec![line];
    }
    // Simple char-split wrapping. No word-boundary awareness needed
    // for code fences.
    let mut out = Vec::new();
    let mut start = 0;
    let mut len = 0;
    for (i, c) in line.char_indices() {
        if len >= max_chars {
            out.push(&line[start..i]);
            start = i;
            len = 0;
        }
        len += 1; // char count; UnicodeWidthStr for display width
    }
    if start < line.len() {
        out.push(&line[start..]);
    }
    out
}
```

Use `unicode_width::UnicodeWidthStr::width()` for display width, not
character count.

#### Acceptance

```rust
#[test]
fn long_code_line_wraps_with_continuation_gutter() {
    let body = "```rust\nlet x = \"this is a very long line that exceeds the viewport width significantly\";\n```\n";
    let lines = render(body, Theme::Dark, Some(40));
    let txt = lines.iter().map(|l| line_text(l)).collect::<Vec<_>>().join("\n");
    let gutter_lines: Vec<_> = txt.lines().filter(|l| l.contains("│")).collect();
    assert!(gutter_lines.len() >= 3, "expected wrapped lines with gutter, got: {}", txt);
}
```

#### Done when

- Code fence `│ ` gutter appears on every wrapped segment.
- No change in behavior when wrap_width is `None` or large.
- Width-aware (e.g. CJK characters count wider).

---

### Phase U5 — Improve inline code styling

**Problem:** `Modifier::REVERSED` is harsh and often unreadable.
**Solution:** Subtle background pill.

#### Files

- `src/tui/markdown.rs`:
  - Change `current_inline_style()` for `code > 0`:
    ```rust
    if self.code > 0 {
        s = s.bg(Color::DarkGray).fg(Color::White);
    }
    ```
  - Remove `REVERSED` usage entirely.

#### Fallback for terminals that don't support bg colors

Most modern terminals support `bg()` in 2024. The core 16 ANSI
background codes are universally supported. Keep this simple.

#### Acceptance

```rust
#[test]
fn inline_code_has_bg_not_reversed() {
    let lines = render("call `Read` to load", Theme::Dark, None);
    let span = first_span_with_text(&lines, "Read").expect("code span");
    assert!(
        span.style.bg == Some(Color::DarkGray),
        "expected DarkGray bg, got {:?}",
        span.style.bg
    );
    assert!(
        !span.style.add_modifier.contains(Modifier::REVERSED),
        "should not use REVERSED"
    );
}
```

#### Done when

- Inline `code` has dim gray background, white foreground.
- Existing inline-code test updated to assert `bg` instead of `REVERSED`.
- Visually readable on both light and dark terminal themes.

---

### Phase U6 — Plain REPL markdown formatting

**Problem:** `oli --plain` shows raw markdown.
**Solution:** Add a lightweight ANSI markdown renderer for the plain
REPL path.

#### Approach options

**Option A (preferred): termimad**
- Pros: purpose-built for this, handles tables/bold/italic/code blocks
  already, small dep (`pulldown-cmark` + `crossterm`).
- Cons: new dependency, may duplicate some of our markdown logic.

**Option B: reuse `markdown::render` + ANSI conversion**
- Convert the existing `Vec<Line>` from `markdown::render` into ANSI
  escape sequences using a small utility.
- Pros: zero new deps, consistent with TUI rendering.
- Cons: need to build an ANSI serializer for ratatui `Line`/`Span`;
  wrapping logic must be reimplemented since stdout has no ratatui
  `Wrap`.

**Decision: Option A (termimad).** It is purpose-built, actively
maintained, and gives us tables + syntax highlighting for plain mode
for free. The dep is small and well-scoped.

#### Files

- `Cargo.toml`:
  ```toml
  [dependencies]
  termimad = { version = "0.31", optional = true }

  [features]
  # ... existing features ...
  # termimad is added to the 'default' feature set
  ```
  Wait — we need to think about feature flags. Adding termimad under
  `default` increases binary size. Put it behind a new `pretty-plain`
  feature or just gate it behind `default` since the default already
  ships syntect (~2MB) and ratatui.

  **Decision:** Add `termimad` as a non-optional dep since the default
  binary already carries TUI + syntect. The `--no-default-features`
  line-mode binary is explicitly minimal by design and doesn't need
  formatting.

- `src/repl/mod.rs`:
  - Import `termimad`.
  - In `run_turn`, wrap the streamed output through termimad rendering
    before `print!("{}", s)`.
  - For streaming: accumulate chunks, re-render the accumulated markdown
    to ANSI, overwrite the previous output using cursor-up + clear-line
    sequences.

- `src/repl/mod.rs` — `ProgressHook` update:
  Render tool card header via termimad (bold tool name, dim args).

#### Streaming ANSI approach

```
Chunk 1: "**hello**
"
→ print termimad::text("**hello**\n")  → "\x1b[1mhello\x1b[0m\n"

Chunk 2: "**hello** world
"
→ CSI 1A (cursor up 1), CSI 2K (clear line)
→ print termimad::text("**hello** world\n") → "\x1b[1mhello\x1b[0m world\n"
```

This is the classic terminal streaming pattern: buffer the full
markdown body so far, re-render each chunk, overwrite the previous
displayed lines. The number of lines to overwrite = previous rendered
line count.

Simplest implementation: collect all chunks into a `String`, run
termimad on the full string, count the emitted lines, on next chunk
cursor-up by `prev_lines` and reprint.

#### Tool card in plain mode

Replace the one-line stderr `→ Tool(args)` with termimad-styled:
```
→ **Read**  src/main.rs  (0.04s) ✓
```

#### Acceptance

- `oli --plain` shows bold text as bold, inline code with dim bg,
  headings with `▌`, code fences with syntect highlighting.
- Piping to a file `oli -p "prompt" > out.txt` strips ANSI or keeps
  raw markdown (decision: keep raw markdown, the pipe case is
  explicitly non-interactive).
- `cargo test` passes.

#### Done when

- `oli --plain` renders markdown with ANSI formatting.
- `--no-default-features` binary still works (termimad is in the
  default feature set only; gate via `cfg`).

---

## Cross-cutting concerns

### Feature-flag impact

- Phase U1–U5 are in the core library, no feature-gating needed.
- Phase U6 (`termimad`) should be available under the `default` feature
  set and gated behind `#[cfg(feature = "termimad")]` or simply
  included when `default` features are on.
  - The `--no-default-features` binary explicitly doesn't get pretty
    output; that's documented behavior.

### Performance

- U1 `strip_ansi_codes` is O(n) single-pass, allocates only when codes
  are present.
- U2 table rendering computes column widths by scanning all rows once.
  Tables in LLM output are typically <20 rows × <8 columns, negligible.
- U6 termimad re-renders the whole markdown body on each chunk. For
  streaming, this is the same O(body) cost as the TUI's re-parse, and
  termimad is fast.

### Existing test updates

- `markdown.rs` tests that call `render(body, Theme::Dark)` must be
  updated to `render(body, Theme::Dark, None)` after the signature
  change in U2.
- The inline-code test changes its assertion from `REVERSED` to `bg`
  in U5.

---

## Acceptance for "pretty output"

A user can:

- Run `cargo test` and all tests pass (no regressions).
- Run a Bash tool that outputs `\x1b[31mred\x1b[0m` and see `red` with
  no escape codes in the TUI transcript and tool card.
- See a markdown table like

  ```markdown
  | Tool | Args | Time |
  |------|------|------|
  | Read | main.rs | 0.04s |
  ```
  rendered with `┌──┬─────┬─────┐` box borders in the TUI.
- See `> quoted text` with a dim `│ ` gutter.
- See `<details>text</details>` rendered with tags and content visible.
- See inline `code` with a dim gray background pill, not inverse video.
- See a long code line wrapped with `│ ` gutter on every continuation
  line.
- Run `oli --plain` and see bold text bold, code blocks highlighted,
  tables rendered.

## Open decisions

1. **U6 termimad dep vs hand-rolled ANSI renderer.** If termimad
   introduces conflicts with existing pulldown-cmark versions, fall back
   to Option B (hand-rolled ANSI serializer for ratatui `Line`).

2. **Wrap width for TUI markdown.** Pass `area.width` directly, or
   leave a small right margin (e.g. `area.width - 2`)?
   - Decision: use `area.width - 2` to avoid edge-case overflow when
     ratatui draws the border of an adjacent widget.

3. **ANSI stripping scope.** Do we strip ANSI from *all* tool results
   (including Read of files that legitimately contain ANSI art), or
   only from Bash/Grep? Decision: strip from Bash and Grep (known
   command-line tools that auto-color), and from MCP tool text blocks
   (servers may return colored terminal output). Read tool is
   opt-in via `strip_ansi` parameter defaulting to false.

4. **Table width limit per cell.** Clip at a hard limit (e.g. 40 chars)
   or proportional to viewport? Decision: proportional. Use
   `max_cell_width = (viewport - 4 - (cols - 1)) / cols`, min 8.

## Status tracker

Mirror commit SHAs into `specs/progress.md` at each phase boundary.

| ID | Item                                              | Status |
| -- | ------------------------------------------------- | ------ |
| U1 | Strip ANSI escapes from Bash/Grep/MCP output      | TODO   |
| U2 | Markdown tables as box-drawing grids              | TODO   |
| U3 | HTML passthrough + blockquote `│ ` gutter         | TODO   |
| U4 | Code fence wrapping with gutter continuation      | TODO   |
| U5 | Inline code: bg pill instead of REVERSED          | TODO   |
| U6 | Plain REPL markdown formatting via termimad       | TODO   |
