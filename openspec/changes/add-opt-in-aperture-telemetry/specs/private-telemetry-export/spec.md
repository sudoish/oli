## Purpose

Provide private, opt-in operational observability for oli sessions without exporting coding content, credentials, or other sensitive session data.

## ADDED Requirements

### Requirement: Telemetry is explicitly and independently enabled
The system SHALL compile telemetry support only when its optional build feature is selected. A build with that feature SHALL keep telemetry disabled unless the effective telemetry configuration explicitly enables it. A build without the feature SHALL continue normal operation when telemetry configuration is present and SHALL report that export is unavailable rather than attempting network export.

#### Scenario: Default configuration
- **WHEN** oli starts without telemetry explicitly enabled
- **THEN** it SHALL create no telemetry exporter and make no telemetry network request

#### Scenario: Enabled configuration in a telemetry-capable build
- **WHEN** oli starts with the telemetry build feature and `telemetry.enabled` set to true
- **THEN** it SHALL export telemetry to the configured private collector

#### Scenario: Enabled configuration in a build without telemetry support
- **WHEN** oli starts with `telemetry.enabled` set to true but without the telemetry build feature
- **THEN** it SHALL continue running without export and report that telemetry support is unavailable

### Requirement: Telemetry configuration is safe and validated
The system SHALL accept telemetry enablement, service name, collector endpoint, protocol, sample ratio, and a header environment-variable name through configuration. It SHALL reject an invalid endpoint, unsupported protocol, invalid sample ratio, or an enabled configuration without an endpoint before attempting export. Authentication values SHALL be read only from the named environment variable and SHALL NOT be persisted in project or user configuration.

#### Scenario: Invalid enabled configuration
- **WHEN** telemetry is enabled with an invalid endpoint or sample ratio outside the inclusive range from zero through one
- **THEN** oli SHALL not initialize an exporter and SHALL present a configuration diagnostic without exposing any credential value

#### Scenario: Environment-provided authorization
- **WHEN** enabled telemetry configuration names an environment variable containing collector headers
- **THEN** oli SHALL use those headers only for collector authentication and SHALL NOT display or export their values as telemetry attributes

### Requirement: Exported telemetry excludes session content and secrets
The system SHALL NOT export user prompts, assistant messages, model request or response bodies, tool arguments, tool results, shell commands, file paths, raw provider errors, API keys, authentication headers, session transcripts, or persistent session identifiers. It SHALL use only documented, low-cardinality operational fields; host identity SHALL be excluded unless separately and explicitly enabled.

#### Scenario: Agent performs a tool call
- **WHEN** an agent invokes a tool with arguments and receives a result
- **THEN** the exported telemetry SHALL include the tool name, duration, policy outcome, and coarse success or failure classification but no arguments or result content

#### Scenario: Model request fails
- **WHEN** a provider request fails with an error containing request content or credentials
- **THEN** telemetry SHALL record only a coarse failure classification and SHALL NOT contain the raw error text or sensitive content

### Requirement: Operational lifecycle data is exported
When telemetry is enabled, the system SHALL emit trace and metric data for the process run, agent runs, model requests, tool calls, policy outcomes, subagent runs, and MCP connections and calls. Model-request telemetry SHALL include provider kind, configured model, streaming state, duration, outcome, and available token counts. Event fields SHALL use bounded values and SHALL NOT use session IDs, paths, or content as dimensions.

#### Scenario: Successful agent run
- **WHEN** an agent completes a user request
- **THEN** the collector SHALL receive a run trace containing child model and tool events with duration and bounded outcome data

#### Scenario: Policy denial
- **WHEN** policy denies or the user declines a tool invocation
- **THEN** telemetry SHALL record the bounded policy outcome and SHALL NOT execute the tool solely for telemetry collection

### Requirement: Export is best effort and non-disruptive
The system SHALL export asynchronously through a bounded buffer. Initialization, export, collector failures, and shutdown flushing SHALL NOT prevent startup, delay normal agent execution beyond the bounded telemetry work, or change the result of an agent, tool, MCP, or provider operation. Export failures SHALL be retained as sanitized local diagnostics and SHALL NOT generate further telemetry.

#### Scenario: Collector is unreachable
- **WHEN** an enabled collector cannot be reached
- **THEN** oli SHALL continue the user session, drop or buffer telemetry within configured bounds, and add a sanitized local diagnostic

#### Scenario: Process exits with pending telemetry
- **WHEN** oli exits while telemetry remains buffered
- **THEN** it SHALL attempt a short bounded flush and then exit regardless of flush success

### Requirement: Telemetry status is safe to inspect
The system SHALL provide a user-accessible telemetry status surface that reports whether telemetry was compiled in, configured, enabled, initialized, and exporting, plus sanitized endpoint and resource information. It SHALL never reveal authentication header values, tokens, prompt content, tool content, or session identifiers.

#### Scenario: User inspects telemetry status
- **WHEN** a user requests telemetry status
- **THEN** oli SHALL show the exporter state and sanitized configuration without exposing credentials or session content
