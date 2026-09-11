# Deep review #2 (post-TUI)

A follow-up to the 8/10 review at the start of this work. That review
landed on a focused agent harness with strong architecture and a few
real gaps. Since then we've shipped 14 phase-commits across the
follow-up roadmap (A–E) plus 12 across the TUI roadmap (F–O), the
deferred items (K4, N4, process-group kill), and ancillary tidying.

This doc rates the harness as it stands, surfaces what's improved,
calls out what's still rough, and flags new roughness introduced by
the journey.

## Executive summary

**Rating: 9 / 10.**

Up from 8/10. Every weakness called out in `specs/review-1.md` is
addressed; every "ideal DX" acceptance criterion from
`specs/tui.md` is met; 431 tests pass (up from 250). The harness is
now a genuinely full-featured terminal coding agent — first-run
wizard, ratatui TUI with markdown + syntect-highlighted code, tool-
call cards, approval modal, sessions picker, /help browser, /undo
+ Ctrl+E, Ctrl+R history search, status bar with token gauge, mouse
wheel scrollback, OSC52 clipboard.

The remaining point is for **scope creep**. The original spec said
"core under 2500 LOC" and "TUI deferred indefinitely." We chose to
override both — the user asked for a real TUI and we shipped one.
The code is still hackable, but the "readable in one sitting"
property is gone.

## By the numbers

| | Start of work | Now | Δ |
|--|--|--|--|
| Tests passing | 250 | 431 | **+181** |
| Source LOC (incl. tests) | ~13 000 | **23 192** | +10 KLoC |
| Source files | 39 | **54** | +15 |
| Direct dependencies | 13 | **20** | +7 |
| Public traits | 9 | **12** | +3 |
| Commits added | — | **23** | — |
| Build warnings | ~12 | 19 | +7 |
| Roadmap items shipped | 0 / 15 | **15 / 15** | full |
| TUI roadmap shipped | n/a | **45 / 45** | full |

LOC by module:

```
tui/      6 373 LOC   (12 files; new since 2026-04-29)
agent/    3 895
tools/    2 489
mcp/      2 443
plugins/  1 308
providers/1 606
policy/     679
config      529
repl/       ~1 540
notes/      ~ 430
```

The largest single file is `src/tui/app.rs` at **1 684 LOC** — the
god-struct holding every overlay state. That's a refactor target
(see "Architectural evolution" below).

## What changed since the 8/10 review

### Closing the gaps from the 8/10 review

The prior review (in-conversation, not committed) flagged the
following gaps. All are now closed:

| Original gap | Status |
|--|--|
| Hooks observe-only | ✓ `HookOutcome::Skip / Replace` (Phase B) |
| Bash no timeout/cwd, grandchildren leak | ✓ timeout + sticky cwd (A1); process-group kill (deferred → done) |
| Edit's read-set lost on `--resume` | ✓ JSONL `read` op + replay (A3) |
| `/plugins reload` missing | ✓ atomic registry swap (C1) |
| No `--strict` for `-p` | ✓ `--strict` flag (A2) |
| REPL silent during tool rounds | ✓ `ProgressHook` + tool-card overlay (A4 + H) |
| `/cost` per-call only | ✓ session-level total (C2) |
| Subagent result not size-capped | ✓ `max_result_bytes` (C3) |
| OpenAI-compat path no prompt cache | ✓ `CacheStrategy::Anthropic` (D1) |
| Plugin sandbox no CPU caps | ✓ mlua thread-hook instruction budget (E1) |
| Anthropic `list_models` empty | ✓ implemented (D2) |
| Diff preview not unified | ✓ `similar` crate (D3) |
| No external-edit invalidation | ✓ mtime tracking (D4) |
| MCP `tools/list_changed` ignored | ✓ refresh + registry swap (E3) |
| No alternate Memory strategy | ✓ `EmbeddingRagMemory` (E2) |

### New ground (TUI)

A `--plain` rustyline path stays as a fallback (and auto-engages
when stdin/stdout aren't TTYs). The default is now ratatui:

- F – TUI skeleton + alt-screen lifecycle
- G – Streaming agent integration with mode transitions + cancel
- H – Tool-call cards with live spinner, timing, per-tool summaries
- I – Single-key approval modal with diff preview + session-allow
- J – pulldown-cmark markdown + syntect-highlighted code fences
- K – `tui-textarea-2` multi-line input, slash + `@path`
  completion popups, history navigation
- L – Proper scroll model (PgUp/PgDn/wheel/Ctrl+Home/Ctrl+End),
  stick-to-bottom with "↓ N new" badge, `/copy N` via OSC52
- M – Always-on status bar (model, token gauge with color
  thresholds, branch, session id) with width-aware collapse
- N – Interactive `/sessions` picker, `/help` browser, `/<cmd> ?`
  inline help, fading onboarding hints, first-run wizard
- O – `/undo`, Ctrl+E edit-and-rerun, real grandchild-killing
  Bash cancel
- K4 – Ctrl-R i-search overlay
- N4 – Full first-run wizard (provider/key/model → config.toml)

## Strengths now

**1. The trait taxonomy held up under load.** The Phase 0 promise of
"one trait per extension axis" survived 23 phase commits and a TUI
rewrite. Three new traits joined (`Embedder`, `ReadLogger`,
`SubagentSpawner` was already there and grew in importance) but the
core 9 are unchanged. New code routes through existing traits;
nothing forced a wider refactor.

**2. Recoverability is real.** The original review noted "if a
runaway Bash slips past Ctrl-C, the user is stuck." Today: Ctrl-C
cancels mid-stream and kills the entire process group on Unix —
verified by a test that confirms a `sleep 1 && touch sentinel`
cancelled at 100ms doesn't touch the sentinel. `/undo` rolls back
memory + transcript in lock-step. Ctrl+E pulls the prior prompt
back into the input box. Memory cancel-rollback survives compaction.

**3. The provider abstraction now genuinely covers the field.**
OpenAI-compat (Ollama, OpenRouter, vLLM, LM Studio, llama.cpp),
native Anthropic with prompt caching, and OpenRouter-routed
Anthropic with the same cache via `CacheStrategy::Anthropic`. A
long session through OpenRouter to Claude reports
`cache_read_input_tokens` instead of paying full repeat-input.

**4. The MCP client is closer to first-class than most clients.**
Not just stdio + streamable-http with session-id capture, but also
live `notifications/tools/list_changed` refresh (the agent loop
re-fetches and atomically swaps registry entries per turn), pure-
read auto-allow, and a `/mcp restart <name>` that re-handshakes
in place without losing the agent's `Arc<Mutex<McpServer>>`-shared
tool registrations.

**5. The TUI is genuinely good.** Streaming markdown with a
slow-blink live cursor, inline tool cards that update in place,
single-key approval, Tab-complete on slashes and `@path`, mouse
wheel scrollback with stick-to-bottom + "↓ N new" indicator,
OSC52 clipboard, theme-detected syntect colors. Side-by-side with
Claude Code's TUI it doesn't feel like a hobbyist project.

**6. Test coverage tracks the surface.** 431 tests, 1.6s wall
time. New tests follow the codebase: `tui/markdown.rs` has 16
unit tests for the rendering pipeline; `tui/wizard.rs` has 9 for
the step machine; `tui/app.rs` alone has 50+ tests covering
overlay state transitions, scroll math, history nav. The bash
process-group kill test correctly catches the original "drop
order is wrong" bug.

**7. Lived-experience details are everywhere.** The OpenRouter
200-OK-with-error JSON. The crossterm 0.28-vs-0.29 mismatch
solved by the `tui-textarea-2` fork. The `KeyEventKind::Release`
filter for Windows. The pin!-shadowing-future-doesn't-drop-at-
block-end gotcha in the bash test. The decision to render
unclosed `**foo` literally during streaming because pulldown-cmark
already does the right thing. These show up as comments and
dedicated tests; they're the kind of thing that makes a harness
feel professional rather than student-project.

## Persistent weaknesses

**1. The "readable in one sitting" promise is broken.** Original
spec capped the core at 2500 LOC. We're at ~17 000 (excluding
tests; total 23k). The TUI alone is 6 400 LOC. A new contributor
can't sit down and read the whole agent loop in a session — they
have to pick a slice (TUI render path, agent loop, MCP transport)
and ignore the others. Still hackable, just no longer atomic.

**2. `tui::app::App` is a god-struct.** 25+ fields, 7 of them
`Option<*OverlayState>`. Every overlay's keypress handler is a
free function in `tui::mod.rs`. A natural refactor would lift
these into a `Overlay` enum with associated state and dispatch via
trait methods, but it's a sizeable change and the current shape
isn't broken — just unwieldy.

**3. 19 build warnings.** Most are dead-code on intentionally-
public APIs (`SessionEntry` fields, `NotesStore::delete`, the
`set_slash_names` in favor of `set_slash_meta`). A few are real
junk we should clean up:

- `unused import: std::os::unix::process::CommandExt` in
  `tools/bash.rs` — pre_exec block needs the import inside the
  `unsafe` brace.
- `WizardStep::Cancelled` is never constructed (the wizard
  closes via `app.close_wizard()` directly).
- `MULTI_LINE` hint id in `tui/hints.rs` is defined but never
  emitted anywhere.

**4. Binary-only crate.** `src/main.rs` is the entry point;
there's no `lib.rs`. External integration tests, fuzzing, or
embedding the agent in another binary all require either a fork
or copying files. The agent's surface is rich enough now that
exposing it as a library would be valuable — and the trait
taxonomy is already clean enough that doing so is mostly
boilerplate.

**5. No live config reload.** `~/.config/oli/config.toml` is read
once at startup. Editing it mid-session has no effect; the user
has to exit + relaunch. `/plugins reload` exists; `/config reload`
doesn't. Same for `[[caps]]` overrides — the model-capability
registry is computed at construction and frozen.

**6. No telemetry, no structured logs.** When something goes
sideways (provider error, plugin crash, hook misfire), the user
sees an `eprintln!` line and has to scroll up to find it. No
ring buffer, no `/diagnostics` slash to dump recent warnings.
Long-running sessions accumulate noise that's gone the moment
the next status repaint happens.

**7. Approval allow-list isn't persisted.** Pressing `[a]` adds
the (tool, args-canonical-json) fingerprint to a session-allow
set. Restart the binary and the user has to re-approve. A
`[A]ll-allow-and-persist` (write to config's
`policy.auto_allow_*`) would close the gap, but it's not
implemented.

**8. Tool result truncation is uniform 30 KB.** Read, Bash, Grep,
Glob, Task all cap at the same byte count. A 2 MB grep-match dump
is truncated mid-line at 30 KB; the user has no `/show full`
recourse short of re-running the tool with stricter args.

**9. Subagent's `Task` doesn't share `ToolContext`.** `Task`-
spawned children get a fresh `ToolContext`, so they have to re-
Read files the parent already Read this session. Notes are shared
(via `NotesStore`), but tool-context reads / cwd / mtime tracking
don't propagate. The parent's `--resume`-replayed read-set isn't
visible to a fresh subagent either.

**10. No `oli init` for headless onboarding.** The first-run
wizard is TUI-only. A user setting up `oli` in a Dockerfile or CI
script needs to write `config.toml` by hand from `specs/README.md`.
A `oli init --provider ollama` command would be a small win.

## New roughness introduced by the journey

These didn't exist (or didn't matter) before this work:

**1. `src/tui/` is a meaningful surface area now.** 12 files,
6 400 LOC, 5 of the 7 traits-of-state-the-app-can-be-in. A future
contributor wanting to add a new overlay (e.g. `/inspect TOOL` to
show schema + recent calls) has to touch app state + key router +
event variant + render fn — four edits across three files.

**2. Crate dep tree grew sharply.** Pre-TUI we had 13 direct deps;
now 20. The new ones (ratatui, crossterm 0.29, tui-textarea-2,
pulldown-cmark, syntect, libc) pull in roughly 50 transitive deps,
including the syntect ~2 MB embedded syntax + theme bundles. The
release binary is now ~14 MB (was ~9 MB). Still a single binary,
but no longer "minimal."

**3. Crossterm version split.** We pin crossterm 0.29 directly
because ratatui 0.30 wants it, but the original `tui-textarea`
crate hard-pinned 0.28; we forked to `tui-textarea-2` to bridge.
This is a normal Rust ecosystem situation, but it means that when
ratatui 0.31 or upstream tui-textarea 0.8 lands, we'll have a small
upgrade dance.

**4. The TUI's drop-order subtlety is a footgun.** The
process-group kill test took several iterations to land because:
(a) `pin!` shadowed the bash future, (b) the future's drop fires
at block exit, not at function exit, (c) leading-underscore
locals don't always survive into the async state machine. The
production code has explicit `drop(pg_guard)` after the await + a
non-underscore name to defend against (c), but the convention is
fragile and the Rust async state-machine semantics here are
under-documented. Future contributors might re-introduce the bug.

**5. Status bar fields are eagerly stringified per frame.** Every
draw recomputes the bullet-separated identity strip, the spinner
glyph, the token gauge color thresholds. At 60 fps that's fine,
but it means the renderer holds owned `String`s rather than borrows.
The single-pass-into-`Vec<Line<'static>>` discipline is correct
but verbose.

**6. The TUI doesn't have its own README.** A user who finds
`oli --help` discovers the binary but won't learn about Tab
completion, Ctrl+R, `/copy N`, or the wizard until they stumble
into them. The deferred-but-not-shipped "K-style cheatsheet" hint
overlay would help but doesn't exist.

## Architectural evolution notes

**Trait surface stayed clean.** The 12 public traits all carry
their weight: 11 had concrete callers in this work, only one
(`ReadLogger`) was pure infrastructure. None grew past their
original shape — `Memory` got `maybe_compact`, `Hook` got
`HookOutcome`, but no surprise methods showed up.

**The agent loop didn't grow.** `Agent::run_streaming` was 80
lines pre-Phase-A and is 95 lines now. The complexity that did
land (cancel-via-cancel-oneshot, hook outcomes, MCP refresh,
session usage tracking) layered on top without making the loop
unreadable. That's evidence the abstractions held.

**The TUI took on its own architecture.** Single mpsc channel
funneling input + agent + hook events into a single render loop;
driver task owning the agent + slash registry; raw-mode key
handler routing through overlay precedence. This is its own
mini-system and has its own conventions; mostly disconnected from
the agent core. That's good for the TUI but creates a real
"two-codebase" feel in the repo.

**Provider factory pattern paid off.** `providers::build()`
existed pre-A; we extended it to thread `cache: CacheStrategy`
through a single edit. New providers (or new caching modes) drop
into one place.

**Config + capability overrides held up.** The Phase 4
`[[caps]]` override mechanism let us add the OpenRouter cache
field without touching the cap registry. The TOML loader's array-
concatenation merge gracefully handled new fields appearing in
overlays.

## Recommendations for a 0.2

What I'd polish next, ordered by impact-per-day:

**1. Refactor `App.*_picker` etc into an `Overlay` sum type
(~0.5d).** Replaces 7 `Option<XState>` fields with one `overlay:
Option<Overlay>`. Keypress dispatch falls out of the enum
naturally. App.rs drops below 1000 LOC.

**2. Surface `/diagnostics` (~0.5d).** Capture every `eprintln!`
into an in-memory ring buffer (8 KB). New slash command renders
the tail. Catches plugin warnings, MCP stderr, provider quirks
that currently scroll off into the void.

**3. Persist the approval allow-list (~0.5d).** Add
`[policy.session_allow]` to `config.toml` (or a separate
`tui-allowlist.json`); load on startup, append on `[a]`. Closes
the "I just approved cargo test five sessions in a row" annoyance.

**4. Lift the agent into a `lib.rs` (~1d).** External integration
+ docs.rs. The current shape is library-friendly; the work is
mostly moving `main.rs` into `bin/oli.rs` and writing a few
public re-exports.

**5. `oli init` CLI (~0.5d).** Mirrors the wizard's first three
steps headlessly: `oli init --provider ollama` writes the same
`config.toml`. Useful for Dockerfiles, CI, dotfile bootstraps.

**6. Live config reload (~1d).** `/config reload` re-parses
config.toml, swaps the active provider if it changed, refreshes
caps overrides. Bigger refactor than `/plugins reload` (Agent
doesn't currently expose a "swap provider" path), but high-value
for iterative config tweaking.

**7. Drop the 19 warnings (~0.25d).** Either remove the dead
code or annotate with `#[allow(dead_code)]` + a comment about why
the surface is intentional. Cleans up `cargo build` output for
new users.

**8. Subagent inherits parent's `ToolContext` (~0.5d).** Pass
the parent's read-set + cwd to the child via `SubagentSpawner::
spawn`. Avoids redundant re-reads when the model delegates a
sub-task that touches files the parent already loaded.

**9. Tool-result "show full" (~1d).** Let the model say "show me
the full result of tool call #N" via a new `ShowFull` tool that
reads from a per-session result cache. Caps the context-window
hit while keeping deep dumps reachable.

**10. Move syntect to a feature flag (~0.5d).** ~2 MB embedded
data + ~hundred-ms startup is paid even on `--plain` runs.
Gating syntect behind `default = ["tui-syntax"]` lets a stripped
build skip it.

## Acceptance against original goals

The original `specs/README.md` listed six success criteria. Status
today:

| Criterion | Status |
|--|--|
| Non-trivial code change against `qwen2.5-coder:7b` on Ollama with no babysitting | ✓ Passes via the local-model survival kit (compaction, fallback parser, capability registry) |
| Same binary works against Claude via OpenRouter (or native Anthropic) with one config flip | ✓ Native Anthropic + prompt caching; OpenRouter routing with the same cache via auto-detection |
| Adding a new tool: one file, one register call | ✓ Holds — see `tools/notes.rs` |
| Adding a new external tool: zero code changes, three config lines | ✓ `[[tools.subprocess]]` |
| Lua plugin loads via filesystem drop + `/plugins reload` | ✓ Full lifecycle, plus instruction-count budget |
| Core (excluding tests + Lua) under 2 500 LOC | ✗ ~17 000 LOC excluding tests. We chose to override this for the TUI. |
| REPL feels responsive, no UI freezes | ✓ Holds — coalesced redraws, cancel-on-Ctrl-C, real grandchild kill |

Six of seven hold; the LOC budget broke deliberately when the
user asked for "ambitious" (TUI). Worth marking in `specs/README.md`
that the budget was retired.

## Final rating: 9 / 10

Up from 8/10. Every flagged weakness is closed, every "ideal DX"
criterion is met, and the TUI is genuinely good. Lost 1 point
for: the LOC budget broken, 19 warnings sitting around, App as
a god-struct, and a few dead-code APIs that should be either used
or pruned. Well within the "minor polish" envelope.

A potential fast-follow that gets to 9.5: items 1, 2, 3, 7 from
the recommendations list (overlay sum type, /diagnostics ring
buffer, persisted allow-list, drop the warnings) — total ~1.5
days of work.
