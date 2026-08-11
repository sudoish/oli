# Private-agent roadmap

## Purpose

Make oli the inspectable terminal coding agent for private infrastructure:
useful on a laptop, over SSH, on a remote workstation, or across a tailnet,
without making Tailscale a runtime dependency.

This roadmap treats working reference setups and the technical content they
produce as deliverables. It is intentionally not a general coding-agent
feature backlog.

## Product thesis

> oli is the inspectable terminal coding agent for code that lives anywhere.

The product should remain:

- terminal-native, including remote and headless hosts;
- local-first, but able to reach private models and tools;
- provider-independent;
- explicit about its trust and privacy boundaries;
- hackable through its existing Rust, Lua, subprocess, and MCP surfaces; and
- usable without Tailscale, while being especially useful on a tailnet.

Tailscale supplies private reachability, identity, and network policy. oli
continues to speak ordinary application protocols such as SSH, HTTP, MCP,
model APIs, and OTLP.

## Success criteria

The roadmap is successful when:

1. A new user can reproduce three documented private-agent setups.
2. Each setup includes tested configuration, a network diagram, a threat
   model, expected failure modes, and a short demonstration.
3. sudoish has a coherent series of durable engineering articles rather than
   disconnected release posts.
4. Tailscale is relevant because it protects real model, tool, and operations
   traffic, not because oli contains a cosmetic integration.
5. Optional telemetry can demonstrate agent reliability without exporting
   prompts, code, paths, credentials, transcripts, or raw errors.
6. The default oli binary and non-Tailscale workflows remain focused and
   unaffected.

## Scope guardrails

### In scope

- Stabilizing and releasing the headless CLI and line REPL.
- A remote-workstation reference setup.
- A private model-plane reference setup.
- A private MCP service-plane reference setup.
- Privacy-preserving operational telemetry after ingestion validation.
- Reproducible examples, diagrams, demo scripts, and sudoish articles.
- Small product changes required to make a documented workflow reliable.

### Deferred unless a reference setup proves the need

- New provider families.
- General multi-agent orchestration.
- A plugin marketplace.
- IDE integration.
- Hosted or multi-user oli.
- Direct `tsnet` or other Tailscale SDK integration.
- A new full-screen terminal interface without evidence that the text-first
  clients are insufficient.
- Broad telemetry instrumentation before one vertical trace is useful.

## Existing foundation

The old engineering roadmaps are complete and should not be copied into this
project as new work. The relevant shipped capabilities are:

- Headless and line-REPL clients, persisted conversations, and resumability.
- Remote-host ChatGPT authentication through `oli login --paste` and device
  authorization.
- OpenAI-compatible providers for private Ollama, vLLM, LM Studio, and
  llama.cpp endpoints.
- Streamable-HTTP MCP transport and live tool-list refresh.
- Layered project configuration, live reload, diagnostics, policy controls,
  plugins, and hooks.
- A library surface for custom embedding and orchestration.

Current work falls into two categories:

- The persisted headless CLI is the release-blocking product surface.
- `openspec/changes/add-opt-in-aperture-telemetry/` is a proposal, not an
  implementation. Its deployment assumptions must be validated first.

## Linear project shape

Create one Linear project named **oli: private-agent workflows**.

### Live Linear tracking

- Initiative: [oli — Private Agent Workflows](https://linear.app/sudoish/initiative/oli-private-agent-workflows-7c6b0fb98739)
- Project: [oli: Private Agent Workflows](https://linear.app/sudoish/project/oli-private-agent-workflows-41f1d45e0858)
- Track parents: [SUD-187](https://linear.app/sudoish/issue/SUD-187),
  [SUD-188](https://linear.app/sudoish/issue/SUD-188),
  [SUD-189](https://linear.app/sudoish/issue/SUD-189),
  [SUD-190](https://linear.app/sudoish/issue/SUD-190), and
  [SUD-191](https://linear.app/sudoish/issue/SUD-191).

Linear is the status source of truth. The local `PA-*` identifiers below are
retained in the Linear issue titles so repository plans, commits, and content
artifacts can refer to work without depending on workspace-specific IDs.

Suggested project summary:

> Ship and document three reproducible private-agent workflows, then add a
> validated, content-free telemetry slice. Each milestone must produce a
> working artifact and publishable sudoish material.

### Suggested labels

- `area:cli`
- `area:remote`
- `area:model-plane`
- `area:mcp`
- `area:telemetry`
- `area:docs`
- `content:sudoish`
- `partner:tailscale`
- `type:discovery`
- `type:implementation`
- `type:validation`
- `type:content`

Use the `partner:tailscale` label only when Tailscale is materially involved
in the workflow or its network policy. Do not use it on generic oli work.

### Priority and estimates

- **Urgent:** release blockers or privacy/security defects.
- **High:** work required by the active milestone.
- **Normal:** supporting content, examples, and follow-up improvements.
- **Low:** deferred ideas that require evidence from a reference setup.

Use Fibonacci estimates: `1` for a small doc/test adjustment, `2` for a
contained change, `3` for a multi-file feature or complete content artifact,
`5` for a reference setup, and `8` only when the issue should first be split.

## Milestones and ticket backlog

Ticket identifiers below are local planning IDs. Replace them with Linear
identifiers when the issues are created, while retaining the local ID in each
issue description for traceability.

### Milestone 0 — Stable release baseline

**Outcome:** the current user experience is stable enough that every later
demo tests the product rather than terminal rendering bugs.

#### PA-001 — Finish and verify the persisted headless CLI

- Type: implementation
- Priority: Urgent
- Estimate: 3
- Labels: `area:cli`, `type:implementation`
- Depends on: none
- Source change: `openspec/changes/replace-tui-with-headless-cli/`
- Deliverable: persisted, resumable headless commands and an optional line REPL.
- Acceptance:
  - Fresh runs persist a conversation id and resumed runs retain context.
  - Text stdout and JSON output remain machine-clean.
  - Headless approvals never block for input.
  - Focused CLI tests and `cargo test --lib` pass.

#### PA-002 — Cut and document the private-agent baseline release

- Type: validation
- Priority: High
- Estimate: 2
- Labels: `area:docs`, `type:validation`
- Depends on: PA-001
- Deliverable: a tagged release suitable for all reference setups.
- Acceptance:
  - Default and no-default-feature builds pass.
  - Release notes name the remote-auth, private-provider, HTTP MCP, sessions,
    diagnostics, and policy capabilities used by this roadmap.
  - Known limitations are explicit.
  - Every reference setup pins this release or a later one.

#### PA-003 — Add ChatGPT/Codex subscription compatibility to the release gate

- Type: validation
- Priority: High
- Estimate: 2
- Labels: `area:remote`, `type:validation`
- Depends on: the shipped subscription implementation tracked historically by
  SUD-185
- Deliverable: repeatable release evidence that subscription access remains a
  first-class path alongside API-key and local/private-model providers.
- Acceptance:
  - Browser, `--paste`, and device-auth paths have smoke checks appropriate to
    their environments.
  - Token refresh, subscription model discovery, and one real prompt are
    verified.
  - Rejected subscription access names the API-key fallback.
  - API-key and local/private provider regressions pass.
  - Release notes state that oli's third-party subscription-compatible backend
    is not a documented public API and may change.

### Milestone 1 — Remote coding workstation

**Outcome:** a user on a laptop can operate oli on a private remote workstation
without exposing SSH or confusing browser-localhost authentication.

#### PA-101 — Specify the remote-workstation topology and threat model

- Type: discovery
- Priority: High
- Estimate: 2
- Labels: `area:remote`, `partner:tailscale`, `type:discovery`
- Depends on: PA-002
- Deliverable: topology diagram and security assumptions.
- Acceptance:
  - Diagram identifies laptop, remote workstation, model/provider, browser,
    session storage, and trust boundaries.
  - The document distinguishes Tailscale SSH from ordinary SSH over a tailnet.
  - Access policy, lost-device, compromised-host, and credential-storage risks
    are addressed.
  - No public ingress is required by the reference design.

#### PA-102 — Build the reproducible remote-workstation example

- Type: implementation
- Priority: High
- Estimate: 5
- Labels: `area:remote`, `area:docs`, `partner:tailscale`
- Depends on: PA-101
- Deliverable: tested setup using remote oli, `login --paste` or device auth,
  resumed sessions, and OSC52 copying.
- Acceptance:
  - A clean host can follow the instructions without undocumented state.
  - Authentication succeeds when the browser and oli run on different hosts.
  - A session can be disconnected, resumed, and copied from safely.
  - Troubleshooting covers localhost callback confusion, terminal clipboard
    support, and unreachable remote hosts.

#### PA-103 — Publish the remote-workstation sudoish package

- Type: content
- Priority: Normal
- Estimate: 3
- Labels: `content:sudoish`, `partner:tailscale`, `area:remote`
- Depends on: PA-102
- Deliverable: article, diagrams, terminal capture, and short demo outline.
- Acceptance:
  - The article begins with the remote-browser authentication problem.
  - It explains which guarantees come from oli and which come from Tailscale.
  - Every command shown is exercised against the reference setup.
  - The artifact links to versioned example configuration.

### Milestone 2 — Private model plane

**Outcome:** oli reaches a model on a private GPU node using an ordinary
OpenAI-compatible endpoint protected by tailnet policy.

#### PA-201 — Specify the private model-plane topology and access policy

- Type: discovery
- Priority: High
- Estimate: 2
- Labels: `area:model-plane`, `partner:tailscale`, `type:discovery`
- Depends on: PA-002
- Deliverable: reference topology for oli and an Ollama or vLLM node.
- Acceptance:
  - MagicDNS naming, endpoint protocol, listening interface, and TLS choice
    are explicit.
  - The policy permits only intended oli clients to reach the model port.
  - Model API authentication limitations are not hidden by network security.
  - Failure cases cover DNS, policy denial, model absence, and slow startup.

#### PA-202 — Add and test a MagicDNS provider example

- Type: implementation
- Priority: High
- Estimate: 3
- Labels: `area:model-plane`, `area:docs`, `partner:tailscale`
- Depends on: PA-201
- Deliverable: versioned example configuration and verification recipe.
- Acceptance:
  - The example uses the existing OpenAI-compatible provider surface.
  - No Tailscale-specific code path is added to oli.
  - The setup validates model listing or a one-shot prompt and a tool-using
    session.
  - Public exposure checks and rollback instructions are included.

#### PA-203 — Publish the private-model sudoish package

- Type: content
- Priority: Normal
- Estimate: 3
- Labels: `content:sudoish`, `partner:tailscale`, `area:model-plane`
- Depends on: PA-202
- Deliverable: article, network diagram, benchmark notes, and demo outline.
- Acceptance:
  - The story centers on separating the agent client from scarce GPU compute.
  - Latency, model startup, privacy, and network-policy tradeoffs are measured.
  - Claims distinguish a private path from end-to-end application security.

### Milestone 3 — Private MCP service plane

**Outcome:** oli invokes an internal tool over streamable HTTP while the tool
remains reachable only by the intended tailnet identities.

#### PA-301 — Choose and threat-model one useful remote MCP service

- Type: discovery
- Priority: High
- Estimate: 2
- Labels: `area:mcp`, `partner:tailscale`, `type:discovery`
- Depends on: PA-002
- Deliverable: one bounded service use case, such as internal documentation
  search or read-only operational status.
- Acceptance:
  - The service is useful enough to justify being remote.
  - Its data classification, tool permissions, authentication, and policy
    boundary are explicit.
  - Mutating production administration is excluded from the first example.

#### PA-302 — Build and validate the private HTTP MCP example

- Type: implementation
- Priority: High
- Estimate: 5
- Labels: `area:mcp`, `area:docs`, `partner:tailscale`
- Depends on: PA-301
- Deliverable: runnable service, oli configuration, and access policy.
- Acceptance:
  - oli connects, lists tools, calls the selected tool, and observes a live
    tool-list refresh.
  - An unauthorized tailnet identity cannot reach or invoke the service.
  - Tool results remain subject to oli policy and truncation behavior.
  - Service outage and reconnect behavior are documented.

#### PA-303 — Publish the private-tool-plane sudoish package

- Type: content
- Priority: Normal
- Estimate: 3
- Labels: `content:sudoish`, `partner:tailscale`, `area:mcp`
- Depends on: PA-302
- Deliverable: article, diagrams, demo, and reusable example.
- Acceptance:
  - The article explains why MCP is the protocol and Tailscale is the network
    boundary.
  - It covers both agent policy and network policy without conflating them.
  - Commands and failure demonstrations are reproducible.

### Milestone 4 — Privacy-preserving agent operations

**Outcome:** prove that useful reliability data can be exported to a private
collector without exporting coding-session content.

#### PA-401 — Validate Aperture OTLP ingestion independently of oli

- Type: discovery
- Priority: High
- Estimate: 3
- Labels: `area:telemetry`, `partner:tailscale`, `type:discovery`
- Depends on: PA-002
- Deliverable: a recorded successful minimal OTLP HTTP/protobuf export.
- Acceptance:
  - Exact endpoint path, protocol, TLS trust model, and header encoding are
    known.
  - Tailnet reachability and access policy are tested.
  - Failure and retry behavior are observed.
  - No oli dependency is added during this ticket.

#### PA-402 — Freeze the safe telemetry field dictionary

- Type: discovery
- Priority: High
- Estimate: 2
- Labels: `area:telemetry`, `type:discovery`
- Depends on: PA-401
- Deliverable: reviewed names, types, cardinality rules, and privacy rules for
  the first trace.
- Acceptance:
  - The first trace is limited to process, agent run, and model request.
  - Every field has a cardinality bound and a reason for collection.
  - Model identifiers are normalized or explicitly accepted as user-defined.
  - The decision about host or random installation identity is explicit.
  - Prohibited content has payload-level regression fixtures.

#### PA-403 — Implement the minimal telemetry vertical slice

- Type: implementation
- Priority: High
- Estimate: 5
- Labels: `area:telemetry`, `type:implementation`
- Depends on: PA-402
- Source proposal: `openspec/changes/add-opt-in-aperture-telemetry/`
- Deliverable: optional process → agent → model trace export and sanitized
  status surface.
- Acceptance:
  - Compile-time and runtime opt-in both default to off.
  - A build without telemetry support makes no exporter request.
  - Export is asynchronous, bounded, and non-disruptive.
  - `/telemetry` reveals state but not secrets or session identity.
  - Payload tests prove that prompts, messages, paths, credentials,
    transcripts, raw errors, and provider bodies are absent.
  - Default, no-default-feature, and telemetry build/test combinations pass.

#### PA-404 — Evaluate the vertical slice before expanding coverage

- Type: validation
- Priority: High
- Estimate: 2
- Labels: `area:telemetry`, `type:validation`
- Depends on: PA-403
- Deliverable: written go/no-go decision for tool, policy, subagent, and MCP
  instrumentation.
- Acceptance:
  - At least one real failure can be diagnosed using the collected data.
  - Runtime overhead, binary-size impact, dropped data, and shutdown behavior
    are measured.
  - Privacy fixtures are inspected at the collector.
  - Follow-up telemetry issues are created only for fields proven useful.

#### PA-405 — Publish the private-observability sudoish package

- Type: content
- Priority: Normal
- Estimate: 3
- Labels: `content:sudoish`, `partner:tailscale`, `area:telemetry`
- Depends on: PA-404
- Deliverable: article, schema table, privacy test demonstration, and topology
  diagram.
- Acceptance:
  - The article centers on observing an agent without observing its code.
  - It includes rejected fields and explains why they were rejected.
  - Aperture, OTLP, and Tailscale responsibilities are distinguished.
  - Limitations and residual privacy risks are explicit.

## Content pipeline

Each reference setup produces the same package so content work remains
repeatable and accountable:

1. **Problem statement:** one concrete operational failure or constraint.
2. **Reference implementation:** versioned config and runnable commands.
3. **Architecture diagram:** nodes, protocols, identities, and trust boundaries.
4. **Threat model:** protected assets, trusted parties, and residual risks.
5. **Failure demonstrations:** at least one access denial and one unavailable
   dependency.
6. **Evidence:** tests, captured output, and measurements.
7. **sudoish article:** durable explanation rather than release promotion.
8. **Short demo:** a script or shot list reproducible from a clean setup.
9. **Tailscale relevance review:** confirm the network adds material value and
   that product claims use accurate terminology.

Potential follow-on engineering stories should be tracked separately from the
reference-setup project unless they block a milestone. Good candidates from
already-shipped work include process-group cancellation, remote OAuth,
read-before-edit persistence, live MCP refresh, Lua instruction budgets, and
streaming terminal redraw correctness.

## Linear issue template

Use this body for issues created from the backlog:

```markdown
## Outcome

What user-visible or operational state exists when this issue is complete?

## Context

- Local planning ID: PA-NNN
- Milestone:
- Relevant repository docs:

## Scope

- Included:
- Excluded:

## Deliverables

- [ ]

## Acceptance criteria

- [ ]

## Verification

- Automated:
- Manual:
- Evidence to attach:

## Dependencies

- Blocked by:
- Blocks:

## Content impact

- sudoish artifact:
- Tailscale relevance:
- Claims or terminology requiring review:
```

## Definition of done

An implementation ticket is done only when its behavior is tested, its
reference instructions work from a clean environment, and limitations are
recorded. A content ticket is done only when its commands and claims have been
checked against the corresponding reference setup.

Milestones close only when all of the following exist:

- a reproducible artifact;
- automated verification appropriate to the change;
- a completed manual run;
- captured evidence linked from the Linear issues;
- updated local documentation; and
- the associated content package or an explicit publishing decision.

## Tracking and reporting

- Update the Linear project weekly with completed outcomes, current risk, and
  the next demonstrable checkpoint.
- Keep issue state in Linear once tickets exist; this file remains the product
  intent, dependency map, and ticket seed rather than a second status tracker.
- Record material scope changes in this file and link the modifying commit from
  the Linear project update.
- Do not open tickets for deferred work until a milestone supplies evidence
  that it is necessary.

Suggested project progress measures:

- Reference setups reproducible: `0 / 3`.
- Content packages ready: `0 / 4`.
- Milestones closed: `0 / 5`.
- Privacy regressions found: count and severity.
- Setup time from a clean host: measured per reference setup.
- Unresolved release blockers: count.

## Recommended execution order

1. Close Milestone 0.
2. Run Milestones 1 and 2 in sequence so remote-host lessons inform the model
   plane.
3. Build Milestone 3 on the established network and documentation patterns.
4. Run PA-401 as an independent discovery spike; do not begin telemetry
   implementation until it and PA-402 are complete.
5. Publish each content package after its reference setup, rather than holding
   all content for a final launch.
