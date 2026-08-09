## Why

oli has no visibility into agent, model, tool, and MCP reliability outside a local diagnostics buffer. Operators need private observability on their tailnet while retaining an explicit privacy boundary for coding sessions.

## What Changes

- Add compile-time and runtime opt-in telemetry that exports OpenTelemetry trace and metric data to an operator-configured Aperture OTLP endpoint reachable over Tailscale.
- Add telemetry configuration, validation, and a sanitized status surface.
- Record agent runs, model requests, tool and policy outcomes, subagent runs, and MCP lifecycle events using safe, low-cardinality attributes.
- Make export asynchronous and best effort; telemetry failures remain local diagnostics and never interrupt agent work.
- Document deployment, TLS/auth configuration, privacy guarantees, and sampling.

## Capabilities

### New Capabilities
- `private-telemetry-export`: Opt-in, privacy-preserving operational telemetry export to a private OTLP collector.

### Modified Capabilities

- None.

## Impact

Affects Cargo features/dependencies, configuration loading, process startup and shutdown, agent/tool/subagent/MCP execution boundaries, diagnostics, CLI or slash-command status output, and user documentation. Requires an Aperture endpoint that accepts OTLP over HTTP/protobuf, with endpoint-specific TLS and authentication supplied outside project configuration.
