## Purpose

Provide a scriptable, resumable command-line agent interface with stable output while removing Oli's graphical terminal dependency and retaining a simple text REPL for optional interaction.

## ADDED Requirements

### Requirement: A command runs one agent turn without an interactive interface
The system SHALL provide an explicit headless run command that accepts exactly one non-empty prompt from a command argument or non-terminal stdin, executes the normal agent loop, and exits without opening a TUI, REPL, editor, or approval prompt.

#### Scenario: Prompt argument
- **WHEN** a user invokes `oli run --prompt "inspect the repository"`
- **THEN** Oli SHALL execute that prompt to completion and exit

#### Scenario: Piped prompt
- **WHEN** a user pipes a non-empty prompt to `oli run` without a prompt argument
- **THEN** Oli SHALL read the complete prompt from stdin, execute it, and exit

#### Scenario: Missing prompt
- **WHEN** `oli run` receives neither a prompt argument nor non-terminal stdin
- **THEN** Oli SHALL exit non-zero with usage guidance and SHALL NOT wait for interactive input

#### Scenario: Conflicting prompt sources
- **WHEN** `oli run` receives both a prompt argument and piped stdin
- **THEN** Oli SHALL reject the invocation without starting an agent run

### Requirement: Headless conversations are persisted and resumable
The system SHALL assign a stable conversation identifier to every fresh headless run, persist its transcript using the existing session store, and allow a later command to append a prompt to that conversation without losing prior context or the persisted read set.

#### Scenario: Fresh conversation
- **WHEN** a headless run starts without a conversation selector
- **THEN** Oli SHALL create and persist a new conversation identifier before executing the prompt

#### Scenario: Continue by identifier
- **WHEN** a user invokes `oli run --conversation <id> --prompt "continue"` with an existing conversation identifier
- **THEN** Oli SHALL replay that conversation, append the new turn, and return the new final response under the same identifier

#### Scenario: Continue latest conversation
- **WHEN** a user invokes `oli run --continue --prompt "continue"` and at least one persisted conversation exists
- **THEN** Oli SHALL append the prompt to the most recently modified conversation

#### Scenario: Unknown conversation
- **WHEN** a user supplies an unknown or invalid conversation identifier
- **THEN** Oli SHALL exit non-zero without creating a replacement conversation or modifying another transcript

### Requirement: Output is safe for humans and scripts
The system SHALL support text and JSON output modes. Successful text output SHALL contain only the final assistant response on stdout and SHALL report the conversation identifier on stderr. Successful JSON output SHALL contain exactly one valid JSON value on stdout with the conversation identifier, final response, selected provider and model, and available usage data. Diagnostics, progress, and errors SHALL NOT be written to stdout.

#### Scenario: Text result
- **WHEN** a text-mode run completes successfully
- **THEN** stdout SHALL contain the final assistant response and stderr SHALL identify the persisted conversation

#### Scenario: JSON result
- **WHEN** a JSON-mode run completes successfully
- **THEN** stdout SHALL parse as one JSON object containing `conversation_id` and `response` without surrounding progress text

#### Scenario: Failed run
- **WHEN** configuration, provider, agent, or persistence failure prevents completion
- **THEN** Oli SHALL write the error to stderr, return non-zero, and SHALL NOT emit a successful result object

### Requirement: Headless execution never waits for approval
The system SHALL apply the same configured tool policy as other Oli clients while using a non-interactive approval implementation. An operation requiring a human decision SHALL be denied rather than blocking or reading approval input from stdin.

#### Scenario: Automatically allowed tool
- **WHEN** policy automatically allows a tool invocation during a headless run
- **THEN** Oli SHALL execute the tool through the normal policy gate

#### Scenario: Approval required
- **WHEN** a tool invocation requires a human approval decision during a headless run
- **THEN** Oli SHALL deny that invocation without blocking and SHALL keep stdout conformant to the selected output mode

#### Scenario: Strict execution
- **WHEN** a user selects strict mode for a headless run
- **THEN** Oli SHALL require approval for otherwise automatic operations and deny every unresolved approval request non-interactively

### Requirement: Headless and interactive clients share one runtime
The headless command and line REPL SHALL use the same provider construction, model selection, system prompt, tool registry, policy gate, hooks, plugins, MCP connections, notes store, session memory, and project working directory behavior.

#### Scenario: Equivalent configured capabilities
- **WHEN** headless and line-REPL runs start from the same directory with the same configuration
- **THEN** both clients SHALL expose the same configured provider, model, built-in tools, plugin tools, MCP tools, hooks, notes, and policy behavior except for interactive approval and presentation

### Requirement: The graphical terminal UI is removed
The distributed Oli binary SHALL NOT contain or select a ratatui interface, fullscreen or inline viewport behavior, mouse or terminal-image rendering, TUI theme configuration, or TUI-only feature flags. Interactive invocation SHALL use the line REPL.

#### Scenario: Interactive invocation
- **WHEN** a user invokes Oli without a headless prompt in a terminal
- **THEN** Oli SHALL start the line REPL and SHALL NOT initialize alternate-screen, raw-mode, mouse-capture, or ratatui rendering

#### Scenario: Former TUI flags
- **WHEN** a user supplies a removed TUI-only flag such as `--inline`, `--fullscreen`, or `--plain`
- **THEN** Oli SHALL reject the flag through normal CLI validation

#### Scenario: Default build
- **WHEN** Oli is built with its default feature set
- **THEN** the build graph SHALL exclude ratatui, crossterm, TUI text-area, syntax-highlighting, and terminal-image dependencies
