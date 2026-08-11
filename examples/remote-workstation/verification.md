# Remote workstation verification record

Copy this file outside the repository for each clean-host run. Do not commit
hostnames, user identities, session content, credentials, OAuth redirects, or
tailnet diagnostics. Complete every common row. In the provider table, complete
the rows for the selected provider and mark every other row
`N/A (provider not selected)`.

| Field | Value |
|---|---|
| Date | |
| oli version and commit | |
| Workstation OS | |
| Laptop OS and terminal | |
| SSH mode | Ordinary SSH over tailnet |
| Provider path | ChatGPT / OpenRouter / Ollama |

## Results

| Check | Command or action | Expected | Result |
|---|---|---|---|
| Tailnet policy test | Save the example policy after replacing identities | Developer accepted to port 22; an unlisted identity has no grant | |
| Tailnet SSH path | `ssh oli-workstation` | Login reaches the non-root remote account | |
| Public path denied | From a non-tailnet network, attempt the workstation's public address on TCP 22 | No connection | |
| Unauthorized identity denied | From an unlisted tailnet identity, attempt `ssh oli-workstation` | No SSH connection | |
| Version pinned | `oli --version` | `oli 0.1.0` | |
| Conversation persistence | Run `oli run -p`, record its id, disconnect, then use `oli run --conversation <id> -p` | Prior context returns and a new turn succeeds | |

## Selected provider results

Complete the rows for the provider named above. Mark rows for the other two
providers `N/A (provider not selected)`; N/A is not valid for a selected-provider
row.

| Check | Applies to | Command or action | Expected | Result |
|---|---|---|---|---|
| Remote browser login | ChatGPT | `oli login --paste` | Credentials saved on workstation; model list discovered | |
| Device login fallback | ChatGPT | `oli login --device-auth` | Credentials saved without a local callback listener | |
| Subscription check | ChatGPT | `oli login --check` | Refresh, catalogue, and prompt pass | |
| Credential mode | ChatGPT | `stat -c '%a %n' ~/.config/oli/auth.json` | `600` | |
| API-key path | OpenRouter | Start oli with the OpenRouter example and make one prompt | Response succeeds | |
| Local/private path | Ollama | Start oli with the Ollama example and make one prompt | Response succeeds without a hosted-provider credential | |

## Failure evidence

Record sanitized observations for the denied identity, public-path denial,
unreachable-host behavior, and one interrupted/resumed session. Mark the run
incomplete if any common or selected-provider command was skipped. N/A is valid
only for rows belonging to an unselected provider.
