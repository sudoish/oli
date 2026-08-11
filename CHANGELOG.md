# Changelog

## v0.1.0 — Private-agent baseline

The first baseline release for oli's private-agent reference workflows.

### Included

- Remote ChatGPT subscription authentication through browser, pasted redirect,
  and headless device-code flows, with a live refresh/model/prompt release gate.
- ChatGPT subscription, OpenAI-compatible, Anthropic, Ollama, OpenRouter, and
  private OpenAI-compatible provider paths.
- Streamable HTTP and stdio MCP clients, including live tool-list refresh.
- Resumable JSONL conversations through `oli run --conversation`,
  `oli run --continue`, and `/sessions`.
- Runtime diagnostics through `/diagnostics` and resolved paths through `/paths`.
- Automatic tool execution by default, with opt-in granular approval policy,
  Bash allowlists, and a persisted approval list.
- Persisted headless execution through `oli run`, plus an optional line REPL.

### Known limitations

- ChatGPT subscription compatibility uses an undocumented third-party backend
  that may change or be withdrawn. API-key providers remain the supported
  fallback.
- The full browser, pasted-redirect, and device-auth matrix requires manual
  release-time verification with a ChatGPT subscription account.
- MCP reachability does not itself make a service private. Network access and
  oli tool policy must be configured independently.
- Session transcripts are local plaintext JSONL files. oli does not encrypt or
  synchronize them.
- Breaking CLI migration: global `-p`, `--resume`, `--plain`, `--inline`, and
  `--fullscreen` are removed. Use `oli run -p`, `oli run --conversation`, or
  invoke `oli` with no subcommand for the line REPL.
