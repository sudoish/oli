## 1. Telemetry foundation

- [ ] 1.1 Add the optional `telemetry` Cargo feature and feature-gated OTLP HTTP/protobuf dependencies; verify default and no-default-feature builds remain telemetry-free.
- [ ] 1.2 Add layered `TelemetryConfig` parsing, defaults, validation, and tests for disabled, valid enabled, and invalid configurations.
- [ ] 1.3 Implement the feature-gated telemetry facade with sanitized status, resource attributes, sampling, bounded asynchronous export, diagnostics-only exporter failures, and bounded shutdown flush.
- [ ] 1.4 Add tests proving disabled and feature-unavailable configurations make no exporter/network initialization and expose the correct status.

## 2. Safe instrumentation

- [ ] 2.1 Initialize telemetry once during startup with run mode, provider/model, and persisted-session state; arrange bounded shutdown on every execution mode.
- [ ] 2.2 Instrument agent runs and model requests with parent/child trace context, duration, coarse outcome, and reported token counts.
- [ ] 2.3 Instrument tool execution and policy outcomes without recording arguments, results, commands, paths, or raw errors.
- [ ] 2.4 Instrument subagent runs and MCP connections/calls with bounded outcome and duration fields while preserving active trace context.
- [ ] 2.5 Add counters for process, agent, model, tool, policy, subagent, MCP, and bounded exporter outcome data.

## 3. User visibility and documentation

- [ ] 3.1 Add the shared `/telemetry` status command and tests that its output is sanitized in enabled, disabled, unavailable, and failed states.
- [ ] 3.2 Document the feature build, configuration example, OTLP HTTP/protobuf Aperture deployment over Tailscale, environment-injected authentication, TLS, sampling, and privacy guarantees.

## 4. Verification

- [ ] 4.1 Add local OTLP HTTP receiver tests that assert expected lifecycle telemetry and trace parentage.
- [ ] 4.2 Add privacy regression tests that assert prompts, messages, tool payloads, shell commands, paths, raw errors, credentials, headers, and session identifiers are absent from exported payloads and status output.
- [ ] 4.3 Run `cargo test --lib`, `cargo build`, `cargo build --no-default-features`, and telemetry-feature build/test combinations.
