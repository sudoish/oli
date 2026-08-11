## 1. Lock the CLI and session contract

- [x] 1.1 Add failing CLI parsing tests for `oli run`, prompt argument versus piped stdin, `--conversation`, `--continue`, output modes, removed global `-p`, and rejected TUI flags.
- [x] 1.2 Add failing session-resolution tests proving fresh headless runs allocate persisted IDs, known conversations resume, latest continuation resolves deterministically, and invalid IDs do not create transcripts.
- [x] 1.3 Add a serializable completed-run result type and tests for text stdout/stderr separation, stable JSON fields, optional usage, and error output.

## 2. Implement persisted headless execution

- [x] 2.1 Introduce the `run` subcommand and prompt-source resolver, including non-terminal stdin and non-interactive rejection of missing, empty, or conflicting input.
- [x] 2.2 Route fresh and resumed headless commands through `PersistedMemory`, preserving replayed reads and write logging under one conversation ID.
- [x] 2.3 Execute headless turns through the existing provider/tool/policy/hook/plugin/MCP startup path with a non-interactive approver and no progress output on stdout.
- [ ] 2.4 Emit final text or one completed JSON object plus deterministic exit behavior, and add integration tests covering new, resumed, strict, provider-failure, and invalid-session runs.

## 3. Remove the TUI product surface

- [x] 3.1 Change interactive dispatch to the line REPL only and remove `--plain`, `--inline`, `--fullscreen`, viewport selection, TUI wizard selection, and TUI-specific startup branches.
- [x] 3.2 Identify any non-rendering helper with proven shared callers, move it under a neutral module with tests, then delete `src/tui/` and all TUI-only tests without overwriting unrelated working-tree changes.
- [x] 3.3 Remove `tui`, `syntax-highlight`, and `images` Cargo features plus ratatui, crossterm, text-area, markdown-rendering, syntax-highlight, and terminal-image dependencies; update the lockfile mechanically.
- [x] 3.4 Remove TUI-only configuration fields and tests, keeping configuration rejection or migration messaging consistent with the project's existing unknown-field behavior.

## 4. Align the public contract

- [x] 4.1 Update README usage, command tables, examples, architecture map, approvals, sessions, and first-run guidance around `oli run` and the line REPL.
- [x] 4.2 Update AGENTS.md, package description/keywords, cheatsheet, baseline-release gate, roadmap/spec references, and changelog; remove or archive documents whose only subject is the deleted TUI.
- [x] 4.3 Document the breaking migration from `oli -p` to `oli run`, from TUI invocation to the line REPL, and from TUI feature builds to the single text-first build.

## 5. Verify the runtime boundary

- [x] 5.1 Run focused TDD suites followed by `cargo test --lib`, binary CLI integration tests, `cargo build`, and `cargo build --no-default-features` if that feature combination remains meaningful.
- [x] 5.2 Inspect the resolved dependency tree and built binary to prove removed TUI/rendering crates and feature names are absent.
- [x] 5.3 Manually run one fresh text conversation, one JSON conversation, one continuation by ID, one `--continue`, and one line-REPL session against the same configured provider.
- [x] 5.4 Route a two-command resumed conversation through Aperture and verify both commands retain one Oli conversation ID while producing observable model traffic and machine-clean CLI output.
