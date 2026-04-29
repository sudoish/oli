# MCP Client — Spec

A Model Context Protocol client embedded in the harness. External MCP
servers (Linear, Notion, Sentry, GitHub, Playwright, Slack, ...) get
dialed up at startup, their tools enumerated, and exposed as
first-class entries in the existing tool registry. To the agent loop
and the policy gate, an MCP-backed tool is indistinguishable from a
built-in `Read` or a subprocess-lite tool.

This doc is a feature spec; it lives alongside `specs/README.md`.
Status of the work is tracked in `specs/progress.md`.

## Mission

Open the harness to the broader MCP ecosystem without expanding the
core trait surface. One new trait (`McpTransport`) covers the
protocol; everything else reuses the existing `Tool`, `Policy`, and
`Hook` machinery. The headline win: any MCP server in the wild becomes
usable in oli with three lines of TOML.

## Why this, why now

We deferred MCP in the original spec on the bet that subprocess-lite
covered the same need with less surface. That bet half-paid off:
subprocess-lite is great for *one-off* tools you'd write yourself, but
it can't consume the published servers — and the published servers are
where the leverage is. Linear, Notion, Sentry, GitHub, Playwright,
Slack, browser automation, etc. all ship as MCP today.

Subprocess-lite stays. It's still the simplest way to wrap a script
you own. MCP slots in next to it for the "I want to use someone else's
server" path.

## Principles

1. **MCP tools are tools.** They register into the same `Registry`,
   surface through the same `openai_schemas()`, route through the same
   policy gate, fire the same Pre/Post hooks. No parallel dispatch
   path.
2. **Servers are connections, not blessed citizens.** A server is a
   long-lived process the harness manages. It exposes some number of
   tools — those are the only objects the agent ever sees.
3. **Config over code.** A new server is a `[[mcp.servers]]` table.
   No recompile.
4. **Local-first transport first.** Stdio is the default path; HTTP /
   streamable-HTTP is opt-in for hosted servers. SSE arrives behind
   the same `McpTransport` trait.
5. **Failures degrade, don't crash.** A flaky server is a missing
   tool, not an aborted REPL. Startup connects best-effort; runtime
   errors surface as tool results the model can react to.
6. **Policy applies.** Every MCP tool call passes the same gate as
   `Bash` or `Edit`. Servers don't get an exemption because they're
   "external."

## Scope (v1)

- **Client only.** No server side. We don't expose oli as an MCP
  server.
- **Transports:** `stdio` (default), `streamable-http`. SSE deferred
  to v2 unless an early integration demands it.
- **MCP capabilities:** `tools` (call + list). `resources` and
  `prompts` listed in the protocol but **deferred** — see "Phased
  capability surface" below.
- **MCP version:** target the current published spec (2025-06-18 at
  time of writing). Feature-detect via `initialize`'s `protocolVersion`
  exchange; surface a clear error if a server only speaks an older
  version.
- **Auth:** environment variable substitution in headers and env. No
  built-in OAuth flow in v1 — servers that need it can either be
  pre-authenticated by the user (pasted token in env) or delegate to
  their own browser-based flow on first run, the same way they would
  outside oli.

## Out of scope (v1)

- **MCP server mode** (oli serving its own tools to others).
- **Resource subscriptions** (`resources/subscribe`). Real-time push
  doesn't map to a single-shot REPL turn yet.
- **Sampling-from-server.** A server asking *us* for a completion is
  a coupling we don't want yet.
- **Discovery / package managers.** Users name their servers in TOML;
  we do not pull from a registry.
- **OAuth dance.** v2 candidate.

## The `McpTransport` trait

```rust
#[async_trait]
pub trait McpTransport: Send + Sync {
    /// Send a JSON-RPC request and await its matched response. Implementations
    /// own their own request-id allocation and response demuxing.
    async fn request(&self, method: &str, params: Value) -> Result<Value>;

    /// Send a JSON-RPC notification (no response expected).
    async fn notify(&self, method: &str, params: Value) -> Result<()>;

    /// Best-effort shutdown. Stdio impls send `notifications/cancelled`
    /// then close stdin; HTTP impls drop the connection.
    async fn close(&self) -> Result<()>;
}
```

Two impls in v1:

- `StdioTransport` — spawns a child process, talks JSON-RPC framed
  with Content-Length headers (LSP-style) over its stdio. Owns a
  `tokio::sync::oneshot` map keyed by request id for response routing.
- `HttpTransport` — `reqwest` against a `streamable-http` endpoint;
  POSTs requests, parses event-stream responses for the streaming
  case, plain JSON for unary. Reuses oli's existing `reqwest::Client`.

Both transports run a single background task per server reading the
inbound stream and dispatching responses / server-initiated
notifications.

## The `McpServer` runtime

One per configured server. Owns the transport, the negotiated
capability set, the latest `tools/list` snapshot, and a logger for
server-side stderr.

```rust
pub struct McpServer {
    name: String,                 // user-provided identifier ("linear", "playwright")
    transport: Box<dyn McpTransport>,
    capabilities: ServerCapabilities,
    tools: Vec<McpToolMeta>,      // populated by initialize -> tools/list
    health: HealthState,          // Healthy | Degraded { since, reason } | Down
}
```

`HealthState` matters because we don't want one bad server to take
down the REPL. A failing call flips the server to `Degraded` and the
next agent turn proceeds with that server's tools either omitted from
the registry or kept and surfaced as "currently unavailable" tool
results — see open questions below.

## How an MCP tool joins the registry

Each entry in a server's `tools/list` becomes an `McpTool` instance,
boxed into the standard `Box<dyn Tool>` and registered through
`Registry::register_box`. The `Tool` trait fields map cleanly:

| `Tool` trait method | Source |
| --- | --- |
| `name()` | `f"{server_name}__{tool_name}"` (namespaced — see below) |
| `description()` | server-provided `description` |
| `parameters()` | server-provided `inputSchema` (already JSON Schema) |
| `run()` | dispatches to `server.call_tool(tool_name, args)` |

### Naming

Servers can collide on tool names (`get_issue` exists in many). We
namespace with a double-underscore: `linear__get_issue`,
`github__get_issue`. This matches Claude.ai's convention and is
filesystem-safe. The display name in `/tools` shows both the namespace
and the bare name for readability.

### Schema passthrough

MCP tools already ship JSON Schema for their inputs. We pass it
through verbatim to the provider — the same shape `openai_schemas()`
already produces for built-in tools. No schema translation layer.

## Config

```toml
# Per-server entries. Identifier is the table key.
[mcp.servers.linear]
kind     = "stdio"
command  = "npx"
args     = ["-y", "@linear/mcp-server"]
env      = { LINEAR_API_KEY = "${LINEAR_API_KEY}" }

[mcp.servers.playwright]
kind    = "stdio"
command = "npx"
args    = ["-y", "@playwright/mcp"]

[mcp.servers.sentry]
kind        = "streamable-http"
url         = "https://mcp.sentry.dev"
headers     = { Authorization = "Bearer ${SENTRY_TOKEN}" }
# Optional per-server overrides
init_timeout_ms = 5000
call_timeout_ms = 60000

# Optional: filter which tools from this server are exposed to the model.
# Glob patterns; empty `allow` = expose all.
[mcp.servers.linear.tools]
allow = ["get_*", "list_*", "save_issue", "save_comment"]
deny  = ["delete_*"]
```

`${VAR}` expansion happens against the harness's environment at server
spawn; missing variables fail the server's startup with a clear error.

### Project overlay

Project config (`<project>/.oli/config.toml`) can add servers, override
fields on global servers, or disable them via `enabled = false`. The
overlay rules already used for providers/policy apply unchanged.

## Lifecycle

1. **Startup (parallel).** For each enabled server: spawn transport,
   send `initialize`, send `notifications/initialized`, call
   `tools/list`. Timeout: `init_timeout_ms` (default 5s). Failures log
   and mark the server `Down`; the REPL continues.
2. **Tool registration.** Healthy servers' tools register into the
   `Registry` before the agent's first turn.
3. **Per-call.** `Tool::run` calls `server.call_tool(name, args)`,
   bounded by `call_timeout_ms`. Cancellation: if the agent loop is
   canceled (Ctrl-C), the in-flight request is aborted and the
   transport sends `notifications/cancelled` per spec.
4. **Shutdown.** On REPL exit (or `SIGTERM`), each server's
   `transport.close()` runs in parallel with a 1s grace period before
   the harness forcibly terminates lingering child processes.

## Policy & hooks

MCP tools route through the existing gate — no new policy axis.
Suggested defaults (config-tunable):

```toml
[policy.mcp]
default = "ask"             # any MCP tool requires approval first time per session
auto_allow_pure_reads = true # "get_*", "list_*", "search_*" name-prefix heuristic
```

The "approve once per session" pattern fits MCP well because users
typically don't want to micromanage every Linear `get_issue` after the
first. The Approver UX gains a "Always for this server / tool" choice.

`PreToolUse` and `PostToolUse` hooks fire for MCP calls just like
built-in tools. A hook can inspect the namespaced name and gate or
log per-server.

## Failure handling

| Failure | Behavior |
| --- | --- |
| Server fails `initialize` at startup | Log error, mark `Down`, do not register its tools. REPL continues. |
| `tools/list` empty | Server is healthy but contributes nothing. Logged at INFO. |
| Single `tools/call` errors out (server-side) | Surface server's error message as the tool result string; do not flip server health. |
| Transport read returns EOF / connection reset | Mark `Down`. In-flight `oneshot`s wake with a transport-error tool result. The next REPL command retries `initialize` once before giving up for the session. |
| Server stderr (stdio) | Captured to a per-server log buffer; `/mcp logs <server>` slash command surfaces it. Not shown to the model. |
| Call timeout | Cancel notification sent, tool result reads "MCP call timed out after Ns". |

## Slash commands

- `/mcp` — list servers, their health, and tool counts.
- `/mcp logs <server>` — recent stderr / transport-level errors.
- `/mcp restart <server>` — re-run the connect + initialize sequence.
- `/mcp tools <server>` — list tools the server exposes (handy when
  the server has dozens and you want to refine the `allow` filter).

These compose with the existing slash command infrastructure
(`repl/slash.rs`); no new mechanism needed.

## Token cost — the real concern

A typical MCP server exposes 20–80 tools, each with a JSON Schema. A
greedy "register everything from every server" config blows past 30k
tokens of tool definitions for a Claude system prompt. Two
mitigations, both in v1:

1. **`allow` / `deny` per server.** Surface only the tools the user
   needs. The defaults examples in our config docs will lean curated,
   not exhaustive.
2. **Lazy schema loading flag.** Per-server `lazy = true` registers
   the tool *names and one-line descriptions* up front, then fetches
   full input schemas on first use via `tools/get`. Useful for hobby
   integrations; off by default because it changes latency on first
   call.

Anthropic prompt caching mitigates the steady-state cost: tool
definitions are pinned to the cached prefix, so the hit is paid once
per cache window, not per turn.

## Phased capability surface

| Phase | What lands |
| --- | --- |
| 5a | `McpTransport` trait, `StdioTransport`, `McpServer` lifecycle, `tools/list` + `tools/call`, `Tool` impl, `Registry` registration, `[[mcp.servers]]` config, `/mcp` slash. Linear or Playwright as the first integration we exercise end-to-end. |
| 5b | `HttpTransport` (streamable-http), per-server `allow`/`deny` filtering, `/mcp restart`, policy `default = "ask"` + `auto_allow_pure_reads`. |
| 5c | `resources/list` + `resources/read` exposed as a built-in `McpResource` tool (one tool, server+uri arg) so we don't multiply tool count by N resources. `prompts/list` as discoverable slash commands. |
| 5d | OAuth flow (browser-spawn, callback listener) for hosted servers that need it. SSE transport if any target server demands it. |

5a is the only phase that has to land for "amazing" — everything else
is upside.

## Open questions

1. **Down-server tool visibility.** When a server flips to `Down`, do
   we (a) silently drop its tools from the next snapshot, or (b)
   leave them registered and have `run()` immediately return a
   "server unavailable" string? (a) is cleaner; (b) lets the model
   try again without a restart and surfaces the failure to it. Lean
   toward (b) for MCP because servers are flaky and hidden state
   confuses local models.
2. **Namespacing separator.** Double-underscore matches Claude.ai but
   collides with the legal bash identifier `linear__get_issue`. Some
   providers reject this in tool names. Fallback: `:` (used by
   Cursor) or `.` (used by some clients). Default `__`, validate at
   server-load and warn.
3. **Concurrent calls per server.** MCP allows multiple in-flight
   requests with distinct ids. Do we serialize per-server (simpler,
   safer for stateful servers) or pipeline (faster, matches spec)?
   Default: pipeline for HTTP, serialize for stdio (some Node servers
   misbehave under concurrency).
4. **Tool-budget UX.** When a config registers 200+ MCP tools, do we
   warn at load time and require a `--yolo` flag, or just trust the
   user? Lean toward a warning above some threshold (50 tools or
   ~10k tokens of schema), printable via `/mcp budget`.
5. **Hot reload.** Should `/mcp restart <server>` re-pull the tool
   list and update `Registry` mid-session, or require a fresh REPL?
   Mid-session reload is the better UX but means tools can disappear
   between turns, which the model has to be told about. Defer to 5b.

## Non-goals

- **Not full protocol coverage.** We implement the slice that delivers
  tool use. `roots`, `sampling`, `logging`, `progress` notifications
  arrive only when a real integration needs them.
- **Not a competitor to subprocess-lite.** Subprocess-lite stays as
  the path for "I wrote a script and want the model to call it." MCP
  is the path for "someone else published a server."
- **Not bundling servers.** We don't ship Node, Python, or any MCP
  server binaries. Users provide their own runtime — `npx`, `uvx`,
  `docker`, whatever — through the `command` field.
- **Not auto-installing servers.** No registry, no curl-pipe-bash. If
  a v2 wants a `[[mcp.servers]] use = "@linear/mcp-server"` shorthand,
  it builds on a separate package-manager spec.

## Migration impact on existing code

Surprisingly little.

- New module `src/mcp/` with `mod.rs`, `transport.rs`, `stdio.rs`,
  `http.rs`, `server.rs`, `tool.rs`. Estimated ~600 LOC.
- `Config` gains an `mcp: McpConfig` field; `main.rs` builds servers
  before constructing `Agent`.
- `Agent::new` is unchanged. MCP servers attach by registering tools
  into the same `Registry`.
- `policy/mod.rs` gains an MCP-aware default branch (~30 LOC).
- `repl/slash.rs` adds `/mcp` (~80 LOC).
- `tools/subprocess.rs` is untouched.

Total core impact stays within the 2500 LOC spec target if 5c/5d are
deferred.
