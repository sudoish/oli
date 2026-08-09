# Changelog

## v0.1.0 — Private-agent baseline

The first baseline release for oli's private-agent reference workflows.

### Included

- Remote ChatGPT subscription authentication through browser, pasted redirect,
  and headless device-code flows, with a live refresh/model/prompt release gate.
- ChatGPT subscription, OpenAI-compatible, Anthropic, Ollama, OpenRouter, and
  private OpenAI-compatible provider paths.
- Streamable HTTP and stdio MCP clients, including live tool-list refresh.
- Resumable JSONL sessions through `--resume`, `--continue`, and `/sessions`.
- Runtime diagnostics through `/diagnostics` and resolved paths through `/paths`.
- Automatic tool execution by default, with opt-in granular approval policy,
  Bash allowlists, and a persisted approval list.
- Full TUI and smaller line-mode-only release builds.

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
- The no-default-features build is line-mode only and omits the TUI and syntax
  highlighting.

