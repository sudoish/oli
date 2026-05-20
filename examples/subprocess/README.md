# FormatJson — subprocess tool example

A Python script that pretty-prints a JSON document with sorted keys.
Demonstrates oli's "MCP-lite" subprocess pattern: same contract as a
tool over Model Context Protocol minus the protocol negotiation. The
script speaks JSON-over-stdio — arguments arrive on stdin, the result
goes to stdout, errors go to stderr with a non-zero exit code.

## Registration

Add to `~/.config/oli/config.toml` (global) or `.oli/config.toml`
(project), with an absolute path so the tool resolves from any cwd:

```toml
[[tools.subprocess]]
name = "FormatJson"
command = "python3"
args    = ["/absolute/path/to/examples/subprocess/format_json.py"]
description = "Pretty-print a JSON string with sorted keys."

[tools.subprocess.parameters]
type = "object"
required = ["json"]

[tools.subprocess.parameters.properties.json]
type = "string"
description = "The JSON document to pretty-print."

[tools.subprocess.parameters.properties.indent]
type = "integer"
description = "Indent width (0–8). Defaults to 2."
```

Inside a running session: `/config reload` to pick the tool up
without restarting. `/tools` should now list `FormatJson`.

## Testing

Three tiers. Use the lowest one that exercises what you're changing.

### Tier 1 — script directly (fastest, no oli)

Pipe a JSON arguments object to the script. The script must round-trip
to sorted, indented JSON; bad inputs must exit non-zero with stderr.

```sh
# happy path
printf '%s' '{"json":"{\"b\":1,\"a\":2}","indent":2}' \
  | examples/subprocess/format_json.py

# bad JSON → exit 1
printf '%s' '{"json":"not valid json"}' \
  | examples/subprocess/format_json.py; echo "exit=$?"

# schema error → exit 2
printf '%s' '{}' \
  | examples/subprocess/format_json.py; echo "exit=$?"
```

Catches script bugs in milliseconds. No build, no LLM, no provider.

### Tier 2 — Rust integration test (deterministic)

Pinned in `src/tools/subprocess.rs` next to the SubprocessTool
implementation. Verifies the wrapper-to-script handshake:
sorted-keys, indent honored, schema errors surface to the model.

```sh
cargo test --lib subprocess::tests::example_format_json
```

Skips automatically if `python3` isn't on PATH. This is the CI gate
for the tool/script contract.

### Tier 3 — end-to-end through oli + a model

When you want to confirm the model can actually *find and call*
FormatJson — i.e. the description and parameter schema are clear
enough — drive a single-shot prompt:

```sh
./target/debug/oli -p \
  'Call the FormatJson tool with json="{\"banana\":2,\"apple\":1}" and indent=2. Output the tool result verbatim.' \
  --plain
```

The reply should be the keys sorted alphabetically with the requested
indent. If it parrots the input back, the model didn't actually
invoke the tool — try a bigger model. `qwen2.5-coder:7b` was
observed to hallucinate FormatJson output; `qwen3-coder:30b` and
`glm-4.7-flash` dispatch the call cleanly. Gemma 3 has weaker tool
calling and is not recommended for this test.

Verification trick: pass an input whose sort order is obviously not
the input order (e.g. `{"zeta":1,"alpha":2}`). If the reply doesn't
flip the keys, the model didn't actually call the tool.

`-p` mode auto-approves `Ask` policy decisions; the REPL prompts
`[approve] FormatJson [y/N]` on first use of an unfamiliar tool. To
silence that in the REPL, approve once with session-allow, or add an
`auto_allow` entry for `FormatJson` in your config.

## When tier 1 isn't enough

| Symptom | Likely tier to debug |
|---|---|
| Script produces wrong output for a given input | Tier 1 |
| Tool registers but model gets back garbled args | Tier 2 (encoding/dispatch issue) |
| `/tools` doesn't list `FormatJson` after `/config reload` | Tier 2 + check `/diagnostics` for parse errors in `config.toml` |
| Model never reaches for FormatJson on relevant prompts | Tier 3 — tighten description/parameters; try a stronger model |
