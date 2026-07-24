# Codex TUI Reskin

Re-skin oli's TUI to the Codex CLI (`openai/codex`, `codex-rs/tui`) visual
language. Full reskin, every component, implemented as in-place phased
slices inside the existing module structure. Reference implementation:
`codex-rs/tui/src` @ `99744cfe`.

## Locked decisions (from brainstorming)

1. **Scope: full reskin** — transcript, tool cells, composer, status/info,
   approval, pickers, wizard, welcome header.
2. **The top status bar dies.** Identity info moves to a dim footer line
   under the composer. Session id leaves chrome (still in `/sessions`).
3. **Tool cells collapse on completion** (Codex-pure); failures keep a
   short dim error tail.
4. **Approach: in-place, phased, TDD.** Six slices, each lands working and
   tested inside the existing `src/tui/` modules. No parallel UI module,
   no big-bang swap.

## Target shell layout

Vertical bands (replaces status-bar/transcript/activity/input):

```
(transcript — Constraint::Min)
(status row — 1 line, always reserved; blank when idle)
(composer — 1–8 lines, borderless, tinted band)
(footer — 1 line)
```

### Status row

- Idle: blank (row stays reserved to avoid layout jitter).
- Thinking: `• Thinking (2s • esc to interrupt)`; streaming / tool calls:
  `• Working (…)`. Bullet animated via the existing spinner tick;
  `(elapsed • esc to interrupt)` dim.
- Tool running: one dim detail line under it, `  └ <tool> <args>`
  (absorbs the old activity strip).
- Transcript search open: row shows `search: <query> (n/N)` instead.
- Awaiting approval: hidden — the inline approval prompt takes the
  bottom pane.

### Composer

- No border. Whole band painted with `user_band_bg` tint.
- Prompt glyph `›` bold, default fg; dimmed when input disabled.
- Placeholder (dim): `Ask oli to do anything`.
- Continuation lines indent 2 cols, no glyph. 1–8 line textarea as today.

### Footer

- Left: `? for shortcuts` + dim ` · <cwd> · <branch> · <model>`.
- Right: existing color-graded token gauge — `92% context left`, or
  `123k used` when the context window is unknown. Reuses
  `token_gauge_field` logic (green <60%, amber 60–85%, red >85%).
- Width collapse order: drop `? for shortcuts` → drop `branch` →
  center-truncate cwd → drop `model`. Right side never truncates.
- `?` opens the existing help overlay.

Killed: `oli` title chip, top status bar, activity strip, input border.

## Transcript cells

Blank line between all cells. No `▌` role headers, no `→` tool lines.

### User message

- Full-width band on `user_band_bg` (dark: white @ 12% over terminal bg;
  light: black @ 4%). Blank tinted pad line before and after.
- First line prefix `› ` (bold + dim), continuations `  `. Text default fg.
- `@path` mentions/accent spans cyan.

### Assistant message

- No header. First line `• ` dim bullet, body markdown, 2-col hang.
- No streaming cursor glyph — `▍` is removed; in-flight state lives in
  the status row only.
- Reasoning/system-dim content: dim + italic body, same `• ` prefix.

### Tool cells

- Header: `<bullet> <Verb> <args>` — verb bold. Verbs: `Running`→`Ran`,
  `Calling`→`Called` (MCP), `Edited`/`Added`/`Deleted` (file writes),
  `Read`/`Grep`/`Glob` (dim verb, cheap tools).
- Bullet: animated while running; `•` green bold on success, red bold
  on failure.
- While running: dim output head/tail under `  └ ` (first line) / `    `
  (rest), max 5 rows, middle-truncated.
- **On completion the cell collapses to the header line.** Expansion
  reuses the existing focused-card key.
- Failures keep a dim tail (`  └ error: …`, ≤3 lines) so errors are not
  swallowed by the collapse.

### Diff rendering (edit cells expanded + approval)

- Header: `• Edited <path> (+N -M)` — `+N` green, `-M` red. Multi-file:
  `• Edited N files (+A -B)` then per-file `  └ <path> (+n -m)`.
- Diff lines: dim line-number gutter (width = digits of max line no.),
  sign column, syntax-highlighted content, full-width bg tint:
  dark add `#213A2B` / del `#4A221D`; light add `#dafbe1` /
  del `#ffebe9`. Hunk separator `⋮` dim in the gutter.

### Turn separator

One dim full-width `─` rule after turns that did real work, where "real
work" means the turn emitted at least one tool or diff cell. Turns >60s
embed the label: `─ Worked for 2m 31s ─────…`.

### Notices

- Errors: `■ <msg>` red. Warnings: `⚠ <msg>` yellow. System notices stay
  dim italic.

## Overlays

### Approval (centered modal dies → inline bottom pane)

```
Would you like to run the following command?        (bold)

$ rm -rf target/debug                               (syntax-highlighted)

› 1. Yes (y)                                        (selected: cyan+bold row)
  2. No (n)
  3. Allow for this session (a)
  4. Allow always, persisted (A)
  5. Deny for this session (d)

  Press enter to confirm or esc to cancel           (dim)
```

- Edit approvals: `Would you like to make the following edits?` + the
  tinted diff above the options.
- The option set is exactly today's five responses (`ApprovalResponse`:
  yes / no / allow-session / allow-always-persisted / deny-session);
  only the presentation changes.
- Decisions land in the transcript as `✔ You approved …` (green) /
  `✗ …` (red) cells.

### Other overlays

Sessions picker, help browser, history search, copy fallback, wizard,
completion popup: layout and behavior unchanged. Reskin = selection
accent (`›` prefix + cyan+bold row instead of block highlight) and dim
rounded borders where a box exists. Full-screen overlays stay
full-screen.

## Welcome header

First cell of a fresh transcript; dim rounded-border box, inner width
clamped to 56:

```
╭──────────────────────────────────────────────╮
│ >_ oli (v0.x.x)                              │
│                                              │
│ model: kimi-k3        /model to change       │
│ directory: ~/dev/devenv/oli                  │
╰──────────────────────────────────────────────╯
  To get started, describe a task or try one of these commands:
  /help - show key bindings and commands
  /sessions - resume a previous session
  /config - show current configuration
```

`>_ ` dim, `oli` bold, version dim; labels dim; `/model` cyan.

## Theme

`Theme` gains semantic roles (all three presets get values):

| Role | dark | light | dimmed |
|---|---|---|---|
| `user_band_bg` | white @12% blend | black @4% blend | white @6% blend |
| `diff_add_bg` | `#213A2B` | `#dafbe1` | `#1f2a1f` |
| `diff_del_bg` | `#4A221D` | `#ffebe9` | `#2a1f1f` |

Everything else foreground-only. oli's cyan accent stays for
verbs/links/paths (matches Codex's own accent).

## Slices (implementation order, each TDD)

1. **Shell + composer + footer** — layout bands, status row, borderless
   tinted composer, footer with collapse order; top bar/activity strip
   removed.
2. **Message cells** — user band, `• ` assistant prefix, drop `▍`,
     notice glyphs (`■`/`⚠`).
3. **Tool cells** — verb/bullet grammar, live `  └ ` output, collapse on
   completion with failure tail.
4. **Diffs** — gutter/sign/tint rendering for edit cells and approval.
5. **Approval + overlay accent pass** — inline bottom-pane approval,
   `›` selection accent, decision cells.
6. **Welcome + separators + theme roles** — welcome box, turn rule,
   new `Theme` fields wired through all presets.

## Approved deviations from Codex

1. No blinking streaming cursor at all (Codex has none; `▍` removed).
2. oli keeps its cyan accent / theme system rather than adopting Codex's
   exact palette wholesale (they largely overlap anyway).

## Non-goals (YAGNI)

- Shimmering gradient text animation.
- Rotating random composer placeholders.
- Reasoning-effort prompt glyphs (`›`/`»` tiers).
- Ctrl+T transcript mode.
- Update-notice box.
- `• Exploring/Explored` grouping that batches read/search calls into
  one cell — per-tool lines stay.
- Mouse-driven anything; layout/pane rework of full-screen overlays.

## Testing

Per slice, TDD against the existing pure line-builders (the V2
structural-test pattern — tests inspect rendered `Vec<Line>` without a
terminal): footer collapse order, band padding, bullet state
transitions, collapse-on-completion incl. failure tail, diff tint spans,
approval selection rendering, welcome box width clamping, theme preset
values. One manual terminal checklist per slice at the end.
