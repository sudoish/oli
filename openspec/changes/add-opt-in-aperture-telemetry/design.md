## Context

See `proposal.md` for motivation and `specs/private-telemetry-export/spec.md` for the behavioral contract. oli currently has no tracing or OpenTelemetry dependencies. Its local `diagnostics` module writes sanitized warnings to stderr and a bounded in-process ring buffer; that remains the local failure-reporting path. Startup is assembled in `src/bin/oli.rs`; `Agent::run_streaming` owns the provider/tool/policy loop, subagents are created in `src/bootstrap.rs`, and MCP connections and calls have separate boundaries.

The collector runs privately on the tailnet, but Tailscale is network transport rather than a telemetry protocol. The exact Aperture ingestion URL, TLS model, and authorization header format remain deployment inputs, so the implementation must target standard OTLP HTTP/protobuf and leave credentials outside TOML.

## Goals / Non-Goals

**Goals:**

- Add a feature-gated, runtime opt-in OTLP HTTP/protobuf trace and metric exporter.
- Keep exported attributes bounded and content-free by construction.
- Preserve agent behavior when telemetry is disabled, unavailable, misconfigured, or failing.
- Provide a testable abstraction that can use an in-process or local HTTP collector in tests.

**Non-Goals:**

- Capturing prompts, completions, tool payloads, transcripts, paths, raw errors, credentials, or a generic verbose mode.
- Replacing local diagnostics, hook events, provider usage accounting, or existing policy enforcement.
- Operating an Aperture collector, managing Tailscale ACLs, or deciding the operator's TLS and auth policy.
- Supporting OTLP gRPC in the initial implementation.

## Decisions

### Feature-gate the integration and default it off

Introduce a Cargo `telemetry` feature that owns OpenTelemetry, tracing bridge, and OTLP exporter dependencies. Add a `TelemetryConfig` under existing layered configuration with `enabled = false` by default. The no-feature binary parses the section so shared configuration works across builds, but does not construct network clients and exposes an unavailable state.

This retains the minimal default binary and makes all export opt-in. Always compiling exporters was rejected because it increases default footprint and broadens the default operational surface. Runtime-only enablement was rejected because operators need a build-time assurance that telemetry code and dependencies can be absent.

### Use standard OTLP HTTP/protobuf to a configured private endpoint

Target OTLP HTTP/protobuf with an explicit HTTPS endpoint, using the collector hostname provided by the operator (typically MagicDNS). `headers_env` identifies an environment variable supplying authorization headers; TOML holds only its name. Validate endpoints, protocol, and sample ratios before exporter setup.

HTTP/protobuf is easier to place behind a private gateway and local test server than gRPC, while remaining portable across collectors. A custom Aperture client would couple oli to an unverified vendor API, and direct Tailscale integration would conflate transport and observability.

### Centralize safe telemetry behind a small facade

Create `src/telemetry.rs` behind the feature boundary. It owns initialization, status, resource fields, sampling, shutdown, and narrowly typed methods for agent/model/tool/policy/subagent/MCP events. Call sites pass typed operational values rather than generic maps, preventing accidental inclusion of request bodies, arguments, results, paths, or raw errors.

The facade initializes a bounded batch processor and records sanitized exporter errors through `diagnostics::push` without emitting telemetry about those errors. Existing plugin hooks stay independent: they are user-extensible lifecycle notifications and are not a safe telemetry schema.

### Establish trace hierarchy at execution boundaries

Initialize one process/run span in startup after mode, provider, model, and persisted-session state are known. Make each agent run a child span; model calls and tool calls become children of that agent span. Tool telemetry records timing around the existing policy gate and execution path, so denied and declined calls have outcomes without triggering execution. Subagent spans inherit the invoking context; MCP connection and call spans use the active context when one exists.

Emit counters for starts, outcomes, policy decisions, and exporter drops/failures, with no unbounded dimensions. Existing `Usage` data supplies token counts when providers report it.

### Use a sanitized status command

Add a `/telemetry` slash command (and, if CLI command structure permits without duplication, a matching CLI status command) that queries the facade. It reports compiled/configured/enabled/initialized state, service name, protocol, a sanitized endpoint origin, sample ratio, and exporter health. It never reads back header values.

A slash command uses the registry shared by the REPL and TUI. The status surface avoids asking users to infer configuration from diagnostics and is safer than echoing effective configuration wholesale.

## Risks / Trade-offs

- [Aperture endpoint incompatibility] → Confirm its OTLP HTTP/protobuf URL, TLS trust chain, and header syntax during deployment; retain endpoint/protocol validation and test against a local OTLP HTTP receiver.
- [Telemetry dependency size and compile time] → Keep all exporter dependencies behind the optional feature and test the no-default-feature build separately.
- [Exporter backpressure or outage] → Use a bounded asynchronous batch queue, short timeouts, drop accounting, and a bounded shutdown flush.
- [Accidental data leakage from future call sites] → Expose only typed safe fields, review payload tests that assert prohibited values are absent, and avoid generic attribute APIs outside the module.
- [Trace fragmentation across async work] → Explicitly propagate the active context at agent, tool, subagent, and MCP boundaries; correctness is preferred over adding every possible nested event.

## Migration Plan

1. Release with telemetry feature disabled by default and no behavioral change for existing installations.
2. Build an operator binary with `--features telemetry`; configure a private HTTPS OTLP HTTP/protobuf collector endpoint and inject auth headers through the configured environment variable.
3. Verify status and a one-shot run against the tailnet collector, checking that received payloads contain only the documented safe fields.
4. Disable `telemetry.enabled` or deploy a binary without the feature to stop export immediately. Collector unavailability also degrades to local diagnostics without requiring rollback.

## Open Questions

- What exact OTLP HTTP/protobuf base URL/path, TLS certificate model, and authorization-header encoding does the Aperture deployment require? This is an operator deployment value and does not alter the selected protocol or privacy contract.
