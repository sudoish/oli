# Polish roadmap (post-9/10 → 10/10)

Closes the persistent weaknesses called out in `specs/review-2.md`.
Goal: turn a 9/10 "feature-complete with rough edges" harness into
a 10/10 "clean, robust, easy to navigate" one.

The work is sized at roughly **5–6 days end-to-end**. Each phase is
independently shippable; you can stop at any phase boundary and the
codebase stays in a better state than before.

## Goals

1. **Clean.** Zero build warnings. No file past ~700 LOC where the
   domain doesn't justify it. App's overlay state in one
   well-organized module instead of seven scattered `Option<*>`
   fields.
2. **Robust.** Persistent state where users expect it (approval
   allow-list, etc.). Config edits don't require a restart.
   Diagnostics surface what's happening when things go wrong.
   Subagents inherit the parent's tool context so they don't
   re-read files.
3. **Easy to navigate.** `lib.rs` exposes a clear public API
   surface for embedders + docs.rs. Module-level `//!` doc comments
   on every top-level module. A `specs/README.md` table-of-contents
   so a new reader knows where to start.

## Non-goals (explicit)

- Major architecture rewrites. The 12-trait taxonomy holds; we're
  polishing the seams, not redrawing the map.
- TUI feature additions. Phase F–O is closed; this plan is about
  cleaning, not extending.
- Breaking changes to config, slash commands, or plugin contracts.
  Existing user setups must continue to work.
- Performance optimization. The 60fps render budget is met; we
  don't chase micro-wins.

## Phases

### Phase P — Cleanup pass (~1 day)

The fastest readability wins. Land before any deeper work so
follow-up commits aren't fighting noise.

- **P1. Drop the 19 build warnings.** Audit each:
  - Real dead code (e.g. `WizardStep::Cancelled` never
    constructed, `MULTI_LINE` hint never emitted, the
    `unused import: std::os::unix::process::CommandExt` in
    `tools/bash.rs`): prune.
  - Intentional public surface (`SessionEntry` fields,
    `NotesStore::delete`, the deprecated `set_slash_names` in
    favor of `set_slash_meta`): annotate with
    `#[allow(dead_code)]` + a one-line comment about why the
    surface is kept.
  - Done when: `cargo build` and `cargo test` produce zero
    warnings. CI-style green.

- **P2. Overlay sum type.** Replace App's seven `Option<*State>`
  fields with one `pub overlay: Option<Overlay>` enum:
  ```rust
  pub enum Overlay {
      Approval(ApprovalState),
      SessionsPicker(SessionsPickerState),
      HelpBrowser(HelpBrowserState),
      InlineHelp(InlineHelpState),
      HistorySearch(HistorySearchState),
      Wizard(WizardState),
  }
  ```
  Completion menu stays separate (it's not modal — it's an
  in-input affordance). Keypress dispatch becomes a single
  `match` instead of seven `is_some()` checks. Render layer
  follows.
  - Files: `src/tui/app.rs` (state), `src/tui/mod.rs` (key
    routing), `src/tui/ui.rs` (render).
  - Done when: App.rs drops below 1000 LOC; the keypress router
    in `mod.rs` has one overlay-handling branch instead of six.

- **P3. Split the two largest TUI files.** `tui/app.rs` (1684
  LOC) and `tui/ui.rs` (1375 LOC) hold too much. Extract
  cohesive sub-modules:
  - `tui/app/transcript.rs` — TranscriptItem, the streaming +
    tool-card lifecycle methods, scroll math.
  - `tui/app/overlays.rs` — the Overlay enum + each variant's
    state struct.
  - `tui/app/mod.rs` — App, Mode, key routing, submit logic.
  - `tui/render/transcript.rs` — `draw_transcript` + helpers.
  - `tui/render/status.rs` — status bar.
  - `tui/render/overlays/{approval,sessions_picker,help_browser,
    inline_help,history_search,wizard}.rs` — one file per
    overlay's render fn.
  - `tui/render/mod.rs` — top-level `draw` + completion popup.
  - Done when: no `tui/*.rs` file exceeds 500 LOC. Each render
    file is ≤200 LOC.

### Phase Q — Library split (~1 day)

Make the harness embeddable + better documented for external
contributors.

- **Q1. Extract `lib.rs`.** Move the module declarations + their
  parents from `main.rs` to a new `src/lib.rs`. Public re-exports
  the public API: `Agent`, `Provider`, `Tool`, `Memory`,
  `SubagentSpawner`, `Hook`, `Policy`, `SlashCommand`,
  `Config`, `McpHandle`, `EmbeddingRagMemory`, `OllamaEmbedder`.
  Anything internal stays `pub(crate)`.
  - Done when: `cargo doc --no-deps --open` renders a useful
    public API page; `cargo test --lib` passes.

- **Q2. Move binary entry to `src/bin/oli.rs`.** Strips it down
  to ~50 LOC of clap parsing + `oli::tui::run` /
  `oli::repl::run`.
  - Done when: `cargo run --bin oli` starts the same TUI as
    today; `cargo build` produces both a library rlib and the
    binary.

- **Q3. Add module-level `//!` docs to every top-level module.**
  `agent/`, `tools/`, `providers/`, `mcp/`, `policy/`,
  `plugins/`, `tui/`, `repl/`, `notes/`, `hooks/`. One
  paragraph each: what's in here, what trait(s) it exposes, how
  to add a new implementation. Some already have these; finish
  the set.
  - Done when: `cargo doc` produces a module index where every
    entry has a meaningful one-line summary.

### Phase R — Operational visibility (~1 day)

When something goes wrong, the user sees it.

- **R1. `/diagnostics` ring buffer.** Capture every
  `eprintln!`-style operational message (plugin warnings, MCP
  stderr, provider quirks, hook errors) into a per-process
  `Mutex<VecDeque<DiagnosticEntry>>` capped at ~8 KB. New slash
  command `/diagnostics` renders the tail in a paginated overlay.
  - Files: `src/diagnostics.rs` (new), wire into existing
    `eprintln!` sites.
  - Done when: a plugin that emits a `[plugin:foo] info bar`
    message has it visible in `/diagnostics` even if the user
    didn't see it scroll by.

- **R2. Tiny logging shim.** Replace bare `eprintln!` calls in
  the agent / mcp / plugins / providers crates with a `log!`
  helper that:
  - Pushes the message into the diagnostics ring.
  - Prints to stderr (TUI captures this; line-mode REPL shows
    inline).
  - Honors a `RUST_LOG`-style env var if set, otherwise
    info-level default.
  - No dep on `tracing` — keep it under 100 LOC of internal code.
  - Done when: every operational line in the codebase routes
    through one place; `RUST_LOG=debug oli ...` shows verbose
    output without code changes.

- **R3. `/diagnostics clear`.** Wipes the ring buffer. Useful
  before reproducing a bug. Trivial; lands with R1.

### Phase S — Persistent user state (~1.5 days)

The harness should remember decisions the user has made.

- **S1. Persisted approval allow-list.** Pressing `[A]ll` (the
  caps version of `[a]llow`) on the approval modal now also
  writes the (tool, args-canonical-json) fingerprint to
  `~/.config/oli/policy-allow.json`. On startup, `TuiApprover`
  loads it; subsequent matches auto-resolve as today.
  - Files: `src/policy/persisted_allow.rs` (new), wire into
    `TuiApprover`, modal-render legend update.
  - Done when: `[A]` once on `Edit src/main.rs`; restart the
    binary; same Edit auto-allows. `~/.config/oli/policy-allow.
    json` survives.

- **S2. Subagent inherits parent's `ToolContext`.** Currently
  `AgentSpawner::spawn` builds a fresh `ToolContext` for the
  child. Change `SubagentSpawner::spawn` to optionally accept a
  parent context (or a snapshot of relevant state — read-set,
  cwd). The spawner clones the parent's context so the child
  starts with the parent's read history.
  - Trade-off: the child can also `mark_read` files; do those
    propagate back? Decision: no — child reads are local. The
    spawn is a one-way snapshot.
  - Done when: a parent that did `Read("src/main.rs")` then
    spawned a Task can have the subagent immediately
    `Edit("src/main.rs", ...)` without re-reading.

- **S3. Tool-result `ShowFull` tool.** Every tool result that
  hits the 30 KB truncation cap stores the full body in a
  per-session result cache keyed by tool-call id. New tool
  `ShowFull(id: usize)` reads from the cache. The model can
  pull deeper detail when it actually needs it without
  blanket-loading the trimmed content into context.
  - Files: `src/tools/show_full.rs` (new),
    `src/tools/util.rs` (truncation captures body),
    `src/tools/context.rs` (cache).
  - Done when: a `Bash(ls /usr/share)` returning 200 KB
    truncates as today, but a follow-up
    `ShowFull(<id>, offset=30000, limit=20000)` returns the
    next 20 KB.

### Phase T — Onboarding & packaging (~1.5 days)

The "easy to navigate" piece for new contributors and operators.

- **T1. `oli init` CLI subcommand.** Headless mirror of the TUI
  wizard:
  ```sh
  oli init                                    # interactive prompts on stdin
  oli init --provider ollama                  # all defaults, no prompts
  oli init --provider openrouter --api-key … # full non-interactive
  ```
  Writes the same `~/.config/oli/config.toml` the TUI wizard
  does; refuses to clobber an existing file unless `--force`.
  - Files: `src/bin/oli.rs` (new subcommand wiring),
    `src/wizard_init.rs` (shared with the TUI wizard).
  - Done when: a fresh Dockerfile with `RUN oli init --provider
    ollama` produces a working config.

- **T2. Live config reload.** New `/config reload` slash
  command. Re-parses `config.toml`, swaps the active provider
  if it changed, refreshes `[[caps]]` overrides, applies new
  `[policy]` settings.
  - Trade-offs: switching providers mid-session is fiddly (the
    Agent owns a `Box<dyn Provider>`). Use the existing
    `providers::build` factory + `Agent::with_provider()`
    builder + `Agent::with_caps()`. Memory survives; pinned
    system prompt survives.
  - Done when: edit `config.toml` to change `default_model`;
    `/config reload`; `/cost` shows the new model name in the
    next response.

- **T3. Syntect feature flag.** `[features]` in `Cargo.toml`:
  - `default = ["tui", "syntax-highlight"]`
  - `tui = []` — gates ratatui, crossterm, tui-textarea-2,
    pulldown-cmark.
  - `syntax-highlight = ["dep:syntect"]` — gates syntect.
  - `cargo build --no-default-features` produces a
    `--plain`-only binary with no ratatui or syntect (~2-3 MB
    smaller).
  - Done when: feature combinations build and test cleanly;
    release binary still ships full TUI.

- **T4. TUI cheatsheet doc.** New `docs/cheatsheet.md` (or
  inline in README) listing every keyboard affordance: Ctrl+C
  cancel, Ctrl+D / `:q` quit, Ctrl+E edit-and-rerun, Ctrl+R
  history search, PgUp/PgDn / Ctrl+Home / Ctrl+End scroll,
  Tab autocomplete, `@path`, `/copy N`, `/undo`, `/cost ?` etc.
  Linked from `oli --help` output.
  - Done when: a new user can find every keyboard feature in
    one place; the cheatsheet is current.

- **T5. `specs/README.md` table of contents.** A landing page
  pointing at:
  - `specs/README.md` (overview)
  - `specs/progress.md` (state)
  - `specs/roadmap.md` (post-MCP follow-ups)
  - `specs/tui.md` (TUI architecture)
  - `specs/review-2.md` (latest review)
  - `specs/polish.md` (this doc)
  - `specs/memory.md` (memory strategy)
  - `specs/mcp.md` (MCP design)
  - `specs/ui.md` (historical line-mode UX plan)
  - `docs/cheatsheet.md` (keyboard reference)
  - Done when: the spec dir has a clear "start here" entry.

## Suggested ordering & milestones

- **Day 1: Phase P.** Three commits (P1 / P2 / P3). Cleanup
  before structural work.
- **Day 2: Phase Q.** Library split + module docs. Two commits
  (Q1+Q2 together; Q3 separately).
- **Day 3: Phase R.** Diagnostics infrastructure. Two commits
  (R1+R3 together; R2 separately so the routing change is
  reviewable).
- **Day 4: Phase S, part 1.** S1 (persisted allow-list) + S2
  (subagent ToolContext). Two commits.
- **Day 5: Phase S, part 2 + Phase T, part 1.** S3 (ShowFull) +
  T1 (oli init). Two commits.
- **Day 6: Phase T, part 2.** T2 (config reload), T3 (syntect
  feature flag), T4+T5 (docs). Three commits.

## Acceptance for "10 / 10"

A reviewer reading the codebase fresh sees:

- **Zero build warnings.** `cargo build`, `cargo build --release`,
  `cargo test`, `cargo doc` all green.
- **Largest source file ≤ 700 LOC** (down from 1684).
- **Module index in `cargo doc`** has a one-paragraph summary on
  every top-level module.
- **`lib.rs` exposes a coherent public API.** Embedders can
  `use oli::{Agent, Tool, Provider};` without diving into
  internals.
- **`/diagnostics`** lists recent warnings/errors regardless of
  whether the user saw them scroll by.
- **`[A]ll-allow` persists.** Restart the binary; the user
  doesn't have to re-approve.
- **`oli init`** writes a working config without any TUI.
- **`/config reload`** picks up TOML edits live.
- **`docs/cheatsheet.md`** is the obvious answer to "what
  shortcuts does this thing have?"
- **`specs/README.md`** is the obvious answer to "where do I
  start reading?"

A user runs `oli` with no prior setup, follows the wizard,
discovers `/help` interactively, sees their token gauge in green
on the right, hits Ctrl+R to recall a past prompt, and never has
to leave the TUI to feel productive.

## Open decisions

- **Should the overlay sum type live on App or in `tui/render/`?**
  Lean on App since the keypress router is in `tui/mod.rs` —
  keeping state co-located with its routing simplifies dispatch.
- **Should diagnostics persist to disk?** Lean no for v1 — the
  ring is enough; if a user wants persistent logs they can
  redirect stderr at the shell level. Revisit if `/diagnostics`
  gets heavy use.
- **Should `oli init` also offer a `--reset` mode?** Probably
  yes (delete + recreate config), but as a follow-up. v1 is
  add-only with `--force`.
- **Should we also tackle `tui::app::App` becoming `Send +
  'static` so the full TUI can be embedded in a different
  runtime (e.g. tokio + custom executor)?** Out of scope for
  this polish pass — file under "library polish" if it ever
  becomes a real ask.
- **Should we drop `rustyline` (the `--plain` REPL) entirely?**
  Tempting after Phase F shipped, but `--plain` matters for
  piped-in usage and CI. Keep.

## Status tracker

Mirror commit SHAs into `specs/progress.md` at each phase
boundary.

| ID | Item                                              | Status |
| -- | ------------------------------------------------- | ------ |
| P1 | Zero build warnings                               | DONE   |
| P2 | Overlay sum type on App                           | DONE   |
| P3 | Split tui/app.rs and tui/ui.rs                    | DONE   |
| Q1 | Extract `lib.rs`                                  | TODO   |
| Q2 | Move binary entry to `src/bin/oli.rs`             | TODO   |
| Q3 | Module-level `//!` docs everywhere                | TODO   |
| R1 | `/diagnostics` ring buffer + slash                | TODO   |
| R2 | Tiny logging shim replacing bare `eprintln!`      | TODO   |
| R3 | `/diagnostics clear`                              | TODO   |
| S1 | Persisted approval allow-list                     | TODO   |
| S2 | Subagent inherits parent's ToolContext            | TODO   |
| S3 | Tool-result `ShowFull`                            | TODO   |
| T1 | `oli init` headless CLI                           | TODO   |
| T2 | `/config reload`                                  | TODO   |
| T3 | Syntect (and TUI) feature flags                   | TODO   |
| T4 | TUI cheatsheet doc                                | TODO   |
| T5 | `specs/README.md` table of contents               | TODO   |
