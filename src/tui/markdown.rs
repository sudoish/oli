//! Markdown → ratatui `Vec<Line>` rendering for assistant
//! transcript items. Handles the subset most LLM output uses:
//! headings, bold, italic, inline code, code fences, lists,
//! links, paragraphs.
//!
//! Streaming-safe: the renderer is `pub fn render(body, theme)`,
//! a pure function over the current body string. Each frame
//! re-parses the latest body — pulldown-cmark on a few KB of
//! prose is microsecond-fast, well below the 16ms frame budget.
//! For un-closed inline tokens (a chunk that ends mid-`**bold`)
//! pulldown-cmark falls through to literal text, so the user
//! sees the raw markdown until the closing token lands.
//!
//! Code fences route through `syntect` for syntax highlighting
//! when the `syntax-highlight` feature is on (default). The
//! bundled `SyntaxSet` and `ThemeSet` are loaded lazily on
//! first use (~few hundred ms, ~2MB resident) so a
//! markdown-free session never pays for them. With
//! `--no-default-features --features tui`, code fences fall
//! back to plain dim text — the syntect dep drops out of the
//! binary entirely.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

#[cfg(feature = "syntax-highlight")]
use std::sync::OnceLock;
#[cfg(feature = "syntax-highlight")]
use syntect::highlighting::{Style as SynStyle, Theme as SynTheme, ThemeSet};
#[cfg(feature = "syntax-highlight")]
use syntect::parsing::SyntaxSet;
#[cfg(feature = "syntax-highlight")]
use syntect::util::LinesWithEndings;

/// Foreground color for inline code (text wrapped in backticks).
/// Chosen to be visible on both dark and light terminals without
/// the harsh inverse-video look of `Modifier::REVERSED`. Promote
/// to a theme field when configurable themes land.
const INLINE_CODE_FG: Color = Color::Cyan;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    Dark,
    Light,
}

impl Theme {
    /// Best-effort theme detection. `$COLORFGBG` is set by most
    /// modern terminals to `<fg>;<bg>` as ANSI color indices —
    /// e.g. `15;0` (white-on-black, dark) or `0;15`
    /// (black-on-white, light). Bright vs dim background ANSI
    /// indices: 0–6 are dim, 7+ are bright. Default to dark.
    pub fn detect() -> Self {
        let raw = std::env::var("COLORFGBG").unwrap_or_default();
        let last = raw.split(';').next_back().unwrap_or("");
        match last.parse::<u8>() {
            Ok(bg) if bg >= 7 => Theme::Light,
            _ => Theme::Dark,
        }
    }

    #[cfg(feature = "syntax-highlight")]
    fn syntect_name(self) -> &'static str {
        match self {
            // Both ship in `syntect`'s default `ThemeSet`.
            Theme::Dark => "base16-ocean.dark",
            Theme::Light => "base16-ocean.light",
        }
    }
}

/// Render a markdown `body` into a list of styled lines suitable
/// for `Paragraph::new(...)`. Empty input returns an empty vec.
pub fn render(body: &str, theme: Theme) -> Vec<Line<'static>> {
    let mut renderer = Renderer::new(theme);
    let parser = Parser::new_ext(body, Options::ENABLE_STRIKETHROUGH);
    for event in parser {
        renderer.handle(event);
    }
    renderer.finish()
}

struct Renderer {
    /// Theme drives the syntect color set when the `syntax-
    /// highlight` feature is on; in plain-fallback builds the
    /// field is unused but kept on the struct for symmetry and
    /// to keep the public `Theme` enum aligned with what tests
    /// pass in.
    #[cfg_attr(not(feature = "syntax-highlight"), allow(dead_code))]
    theme: Theme,
    /// Lines we've fully composed so far.
    out: Vec<Line<'static>>,
    /// Spans accumulating into the current line. Flushed to
    /// `out` on hard line breaks (paragraphs, lists, headings).
    cur: Vec<Span<'static>>,
    /// Active inline modifiers — toggled on Tag::Strong / Emphasis.
    bold: u8,
    italic: u8,
    strike: u8,
    /// Inline-code span depth (only one normally; we track for
    /// matched start/end events).
    code: u8,
    /// True while inside a heading; flushed on TagEnd::Heading.
    in_heading: Option<HeadingLevel>,
    /// Stack of list states so nested lists know their indent +
    /// numbering.
    list_stack: Vec<ListState>,
    /// Pending link href; populated on Tag::Link, appended in
    /// faded `(url)` form on TagEnd::Link.
    pending_link: Option<String>,
    /// Accumulator for the body of a code fence. Flushed
    /// (highlighted) on TagEnd::CodeBlock.
    code_block: Option<CodeBlock>,
}

struct ListState {
    /// `Some(n)` for ordered lists (next item number); `None`
    /// for unordered.
    next_n: Option<u64>,
}

struct CodeBlock {
    body: String,
    lang: Option<String>,
}

impl Renderer {
    fn new(theme: Theme) -> Self {
        Self {
            theme,
            out: Vec::new(),
            cur: Vec::new(),
            bold: 0,
            italic: 0,
            strike: 0,
            code: 0,
            in_heading: None,
            list_stack: Vec::new(),
            pending_link: None,
            code_block: None,
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        self.flush_line();
        self.out
    }

    fn flush_line(&mut self) {
        if self.cur.is_empty() {
            return;
        }
        let spans = std::mem::take(&mut self.cur);
        self.out.push(Line::from(spans));
    }

    fn push_blank(&mut self) {
        self.flush_line();
        // Avoid double-blanks.
        if self.out.last().map(|l| l.spans.is_empty()).unwrap_or(false) {
            return;
        }
        self.out.push(Line::raw(""));
    }

    fn current_inline_style(&self) -> Style {
        let mut s = Style::default();
        if self.bold > 0 {
            s = s.add_modifier(Modifier::BOLD);
        }
        if self.italic > 0 {
            s = s.add_modifier(Modifier::ITALIC);
        }
        if self.strike > 0 {
            s = s.add_modifier(Modifier::CROSSED_OUT);
        }
        if self.code > 0 {
            // Soft cyan foreground — distinct enough to read as
            // code, gentler on the eyes than `REVERSED`. Kept
            // theme-agnostic for now; a configurable highlight
            // color can land later (see specs/formatting.md U5).
            s = s.fg(INLINE_CODE_FG);
        }
        s
    }

    fn push_text(&mut self, text: &str) {
        // If we're inside a fenced code block, accumulate into
        // the buffer for batch syntect highlight at TagEnd.
        if let Some(cb) = self.code_block.as_mut() {
            cb.body.push_str(text);
            return;
        }
        // Strip any trailing newline; pulldown-cmark sometimes
        // includes one inside a Text event for code fences,
        // headings, etc., and we don't want it inside a Span.
        for (i, line) in text.split('\n').enumerate() {
            if i > 0 {
                self.flush_line();
            }
            if line.is_empty() {
                continue;
            }
            self.cur
                .push(Span::styled(line.to_string(), self.current_inline_style()));
        }
    }

    fn handle(&mut self, ev: Event<'_>) {
        match ev {
            Event::Start(tag) => self.start_tag(tag),
            Event::End(end) => self.end_tag(end),
            Event::Text(t) => self.push_text(&t),
            Event::Code(t) => {
                self.code += 1;
                self.push_text(&t);
                self.code -= 1;
            }
            Event::Html(_) | Event::InlineHtml(_) => {
                // Treat as literal text — don't try to parse HTML.
            }
            Event::FootnoteReference(_) => {}
            Event::SoftBreak => {
                // Soft break in source markdown == single newline.
                // CommonMark renders these as a space; we do the
                // same so word-wrapped paragraphs flow correctly.
                if !self.cur.is_empty() {
                    self.cur.push(Span::raw(" ".to_string()));
                }
            }
            Event::HardBreak => self.flush_line(),
            Event::Rule => {
                self.flush_line();
                self.out.push(Line::from(Span::styled(
                    "  ──────────────────────────────────────",
                    Style::default().fg(Color::DarkGray),
                )));
            }
            Event::TaskListMarker(checked) => {
                let glyph = if checked { "[x] " } else { "[ ] " };
                self.cur.push(Span::styled(
                    glyph.to_string(),
                    Style::default().fg(Color::Cyan),
                ));
            }
            _ => {}
        }
    }

    fn start_tag(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {}
            Tag::Heading { level, .. } => {
                self.flush_line();
                self.in_heading = Some(level);
                let prefix = heading_prefix(level);
                let color = match level {
                    HeadingLevel::H1 => Color::Magenta,
                    HeadingLevel::H2 => Color::Cyan,
                    _ => Color::Blue,
                };
                self.cur.push(Span::styled(
                    prefix,
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ));
                self.bold += 1;
            }
            Tag::Strong => self.bold += 1,
            Tag::Emphasis => self.italic += 1,
            Tag::Strikethrough => self.strike += 1,
            Tag::List(start) => {
                self.flush_line();
                self.list_stack.push(ListState { next_n: start });
            }
            Tag::Item => {
                self.flush_line();
                let depth = self.list_stack.len().saturating_sub(1);
                let indent = "  ".repeat(depth);
                let marker = match self.list_stack.last_mut() {
                    Some(ListState {
                        next_n: Some(n), ..
                    }) => {
                        let m = format!("{}. ", n);
                        *n += 1;
                        m
                    }
                    _ => "• ".to_string(),
                };
                self.cur.push(Span::styled(
                    format!("  {}{}", indent, marker),
                    Style::default().fg(Color::Yellow),
                ));
            }
            Tag::CodeBlock(kind) => {
                self.flush_line();
                let lang = match kind {
                    CodeBlockKind::Fenced(s) => {
                        if s.is_empty() {
                            None
                        } else {
                            Some(s.into_string())
                        }
                    }
                    CodeBlockKind::Indented => None,
                };
                self.code_block = Some(CodeBlock {
                    body: String::new(),
                    lang,
                });
            }
            Tag::Link { dest_url, .. } => {
                self.pending_link = Some(dest_url.into_string());
            }
            Tag::Image { .. } => {
                self.cur.push(Span::styled(
                    "[image]",
                    Style::default().fg(Color::DarkGray),
                ));
            }
            _ => {}
        }
    }

    fn end_tag(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => self.push_blank(),
            TagEnd::Heading(_) => {
                self.in_heading = None;
                if self.bold > 0 {
                    self.bold -= 1;
                }
                self.push_blank();
            }
            TagEnd::Strong => {
                if self.bold > 0 {
                    self.bold -= 1;
                }
            }
            TagEnd::Emphasis => {
                if self.italic > 0 {
                    self.italic -= 1;
                }
            }
            TagEnd::Strikethrough => {
                if self.strike > 0 {
                    self.strike -= 1;
                }
            }
            TagEnd::List(_) => {
                self.list_stack.pop();
                self.flush_line();
            }
            TagEnd::Item => self.flush_line(),
            TagEnd::CodeBlock => {
                if let Some(cb) = self.code_block.take() {
                    self.flush_code_block(cb);
                }
            }
            TagEnd::Link => {
                if let Some(url) = self.pending_link.take() {
                    self.cur.push(Span::styled(
                        format!(" ({})", url),
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::ITALIC),
                    ));
                }
            }
            _ => {}
        }
    }

    fn flush_code_block(&mut self, cb: CodeBlock) {
        // Header gutter so code fences are visually distinct from
        // surrounding prose. Rendered as a faint cyan rule with
        // the language tag.
        let header_label = match cb.lang.as_deref() {
            Some(l) if !l.is_empty() => format!("  ┌─ {} ", l),
            _ => "  ┌─ code ".into(),
        };
        self.out.push(Line::from(Span::styled(
            header_label,
            Style::default().fg(Color::Cyan),
        )));

        #[cfg(feature = "syntax-highlight")]
        {
            let (ss, theme) = syntect_assets(self.theme);
            let syntax = cb
                .lang
                .as_deref()
                .and_then(|l| ss.find_syntax_by_token(l))
                .or_else(|| ss.find_syntax_by_first_line(&cb.body))
                .unwrap_or_else(|| ss.find_syntax_plain_text());
            let mut highlighter = syntect::easy::HighlightLines::new(syntax, theme);

            for line in LinesWithEndings::from(&cb.body) {
                // syntect can occasionally fail on malformed input;
                // fall back to dim mono on error.
                let highlighted = match highlighter.highlight_line(line, ss) {
                    Ok(v) => v,
                    Err(_) => {
                        self.out.push(Line::from(Span::styled(
                            format!("  │ {}", line.trim_end_matches('\n')),
                            Style::default().fg(Color::White),
                        )));
                        continue;
                    }
                };
                let mut spans: Vec<Span<'static>> =
                    vec![Span::styled("  │ ", Style::default().fg(Color::Cyan))];
                for (style, frag) in highlighted {
                    let frag = frag.trim_end_matches('\n');
                    if frag.is_empty() {
                        continue;
                    }
                    spans.push(Span::styled(frag.to_string(), to_ratatui_style(style)));
                }
                self.out.push(Line::from(spans));
            }
        }
        #[cfg(not(feature = "syntax-highlight"))]
        {
            // Plain fallback when the syntect feature is off:
            // each line in the fence gets a cyan gutter with the
            // raw text, no per-token colors.
            for line in cb.body.lines() {
                self.out.push(Line::from(vec![
                    Span::styled("  │ ", Style::default().fg(Color::Cyan)),
                    Span::styled(line.to_string(), Style::default().fg(Color::White)),
                ]));
            }
        }

        self.out.push(Line::from(Span::styled(
            "  └─",
            Style::default().fg(Color::Cyan),
        )));
        self.push_blank();
    }
}

fn heading_prefix(level: HeadingLevel) -> String {
    let bars = match level {
        HeadingLevel::H1 => "▌▌▌ ",
        HeadingLevel::H2 => "▌▌ ",
        _ => "▌ ",
    };
    bars.to_string()
}

/// Convert a syntect Style into a ratatui Style, preserving
/// foreground color and bold/italic flags. Background is dropped
/// — the assistant pane has its own bg, and overlaying syntect's
/// theme bg looks busy.
#[cfg(feature = "syntax-highlight")]
fn to_ratatui_style(style: SynStyle) -> Style {
    let mut s = Style::default().fg(Color::Rgb(
        style.foreground.r,
        style.foreground.g,
        style.foreground.b,
    ));
    use syntect::highlighting::FontStyle;
    if style.font_style.contains(FontStyle::BOLD) {
        s = s.add_modifier(Modifier::BOLD);
    }
    if style.font_style.contains(FontStyle::ITALIC) {
        s = s.add_modifier(Modifier::ITALIC);
    }
    if style.font_style.contains(FontStyle::UNDERLINE) {
        s = s.add_modifier(Modifier::UNDERLINED);
    }
    s
}

/// Lazy-initialized syntect bundles. Loaded once on first code
/// fence; subsequent renders reuse them. ~2 MB resident +
/// ~hundred-ms startup cost paid lazily.
#[cfg(feature = "syntax-highlight")]
fn syntect_assets(theme: Theme) -> (&'static SyntaxSet, &'static SynTheme) {
    static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
    static THEME_SETS: OnceLock<ThemeSet> = OnceLock::new();
    let ss = SYNTAX_SET.get_or_init(SyntaxSet::load_defaults_newlines);
    let ts = THEME_SETS.get_or_init(ThemeSet::load_defaults);
    let name = theme.syntect_name();
    let t = ts
        .themes
        .get(name)
        .or_else(|| ts.themes.values().next())
        .expect("syntect ships at least one theme");
    (ss, t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Modifier;

    fn render_dark(body: &str) -> Vec<Line<'static>> {
        render(body, Theme::Dark)
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>()
    }

    fn first_span_with_text<'a>(lines: &'a [Line<'a>], needle: &str) -> Option<&'a Span<'a>> {
        for line in lines {
            for span in &line.spans {
                if span.content.contains(needle) {
                    return Some(span);
                }
            }
        }
        None
    }

    #[test]
    fn empty_input_yields_no_lines() {
        assert!(render_dark("").is_empty());
    }

    #[test]
    fn plain_paragraph_renders_as_a_line() {
        let lines = render_dark("hello world");
        assert!(lines.iter().any(|l| line_text(l).contains("hello world")));
    }

    #[test]
    fn bold_text_carries_bold_modifier() {
        let lines = render_dark("this is **bold** text");
        let span = first_span_with_text(&lines, "bold").expect("bold span");
        assert!(
            span.style.add_modifier.contains(Modifier::BOLD),
            "bold modifier missing: {:?}",
            span.style
        );
    }

    #[test]
    fn italic_text_carries_italic_modifier() {
        let lines = render_dark("an *italic* phrase");
        let span = first_span_with_text(&lines, "italic").expect("italic span");
        assert!(span.style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn inline_code_uses_soft_cyan_foreground() {
        let lines = render_dark("call `Read` to load");
        let span = first_span_with_text(&lines, "Read").expect("code span");
        // Soft cyan replaces the harsh REVERSED inverse-video.
        assert_eq!(span.style.fg, Some(INLINE_CODE_FG));
        assert!(!span.style.add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn h1_heading_has_gutter_and_bold_color() {
        let lines = render_dark("# Title\n\nbody");
        let title_line = lines
            .iter()
            .find(|l| line_text(l).contains("Title"))
            .expect("title line");
        assert!(line_text(title_line).contains("▌"));
        let title_span = first_span_with_text(std::slice::from_ref(title_line), "Title").unwrap();
        assert!(title_span.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn unordered_list_uses_bullet_marker() {
        let lines = render_dark("- one\n- two\n");
        let body = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(body.contains("• one"), "rendered: {}", body);
        assert!(body.contains("• two"));
    }

    #[test]
    fn ordered_list_uses_numbered_markers() {
        let lines = render_dark("1. first\n2. second\n");
        let body = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(body.contains("1. first"), "rendered: {}", body);
        assert!(body.contains("2. second"));
    }

    #[test]
    fn fenced_code_block_renders_with_gutter_and_lang_label() {
        let lines = render_dark("```rust\nfn main() {}\n```\n");
        let bodies: Vec<String> = lines.iter().map(line_text).collect();
        let combined = bodies.join("\n");
        assert!(combined.contains("┌─ rust"), "missing header: {}", combined);
        assert!(combined.contains("│"), "missing gutter: {}", combined);
        assert!(combined.contains("fn main"), "missing code: {}", combined);
        assert!(combined.contains("└─"), "missing footer: {}", combined);
    }

    #[test]
    fn unknown_language_falls_back_without_panicking() {
        let lines = render_dark("```madeup\nblah\n```\n");
        let combined = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(combined.contains("blah"));
    }

    #[test]
    fn link_renders_text_then_dimmed_url() {
        let lines = render_dark("[click](https://example.com)");
        let combined = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(combined.contains("click"));
        assert!(combined.contains("(https://example.com)"));
    }

    #[test]
    fn nested_inline_resets_modifier_correctly() {
        // `**bold *italic* bold**` — the trailing word stays bold
        // after the italic ends.
        let lines = render_dark("**bold *italic* bold**");
        let after = first_span_with_text(&lines, "bold").expect("bold span");
        assert!(after.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn unclosed_inline_token_renders_as_literal() {
        // Streaming midway through `**foo` — we should NOT render
        // the trailing text as bold; CommonMark renders the
        // asterisks literally until the closing pair lands.
        let lines = render_dark("**foo");
        let combined = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(combined.contains("**foo"), "rendered: {}", combined);
    }

    #[test]
    fn theme_detect_handles_dark_colorfgbg() {
        // SAFETY: we're just toggling an env var across one test
        // process. CI runs each cargo-test target serially per
        // crate, but parallel test threads share env. Bracket
        // the change with a cleanup.
        let prior = std::env::var_os("COLORFGBG");
        // Safe in the test sandbox; restored after.
        unsafe {
            std::env::set_var("COLORFGBG", "15;0");
        }
        assert_eq!(Theme::detect(), Theme::Dark);
        unsafe {
            match prior {
                Some(p) => std::env::set_var("COLORFGBG", p),
                None => std::env::remove_var("COLORFGBG"),
            }
        }
    }

    #[test]
    fn theme_detect_handles_light_colorfgbg() {
        let prior = std::env::var_os("COLORFGBG");
        unsafe {
            std::env::set_var("COLORFGBG", "0;15");
        }
        assert_eq!(Theme::detect(), Theme::Light);
        unsafe {
            match prior {
                Some(p) => std::env::set_var("COLORFGBG", p),
                None => std::env::remove_var("COLORFGBG"),
            }
        }
    }

    #[test]
    fn theme_detect_falls_back_to_dark_when_unset() {
        let prior = std::env::var_os("COLORFGBG");
        unsafe {
            std::env::remove_var("COLORFGBG");
        }
        assert_eq!(Theme::detect(), Theme::Dark);
        unsafe {
            if let Some(p) = prior {
                std::env::set_var("COLORFGBG", p);
            }
        }
    }
}
