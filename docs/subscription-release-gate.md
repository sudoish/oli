# ChatGPT subscription release gate

Run this gate before cutting a release. It verifies oli's third-party
subscription-compatible backend as a first-class path without treating it as a
documented public OpenAI API. The backend may change or be withdrawn.

## Automated regressions

```console
cargo test --lib
cargo build
cargo build --no-default-features
```

The provider tests cover rejected subscription credentials and their API-key
fallback, plus API-key and local/private-provider construction.

## Live subscription matrix

Each login mutates the same local credential store. Run the rows sequentially
with a test account, recording the date, oli commit, plan, login method, model,
and result outside the repository. Never commit tokens or redirect URLs.

| Environment | Login | Verification |
|---|---|---|
| Local desktop | `oli login` | `oli login --check` |
| Remote host, browser elsewhere | `oli login --paste` | `oli login --check` |
| Headless host | `oli login --device-auth` | `oli login --check` |

`oli login --check` forces a refresh of the stored token, fetches the live model
catalogue, selects a served general-purpose model, and sends one minimal real
prompt. A passing result names the number of discovered models and the model
used. Any failure must remain actionable; if signing in again does not recover,
verify the API-key fallback with an `openai-compat` provider and
`OPENAI_API_KEY`.

For the local/private path, run one prompt against the Ollama configuration in
the README. Do not publish a release until the automated regressions and every
applicable live row pass, or the release notes explicitly name the failed row
as a known limitation.
