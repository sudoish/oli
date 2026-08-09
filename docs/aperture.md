# Use Oli through Aperture

[Aperture by Tailscale](https://tailscale.com/docs/aperture) can proxy
Oli's model requests through a private tailnet endpoint. This gives Aperture
visibility into model usage while Oli continues to run tools locally.

This guide uses a ChatGPT Plus, Pro, Team, or Enterprise subscription. That
path is different from an OpenAI Platform API key:

| | ChatGPT subscription | OpenAI Platform API |
|---|---|---|
| Upstream endpoint | `chatgpt.com/backend-api/codex` | `api.openai.com/v1` |
| Credential | ChatGPT OAuth token | OpenAI API key |
| Aperture mode | Passthrough | Injected key or passthrough |
| Oli provider kind | `openai-chatgpt` | Depends on the upstream API |

Do not substitute a placeholder API key for the subscription credential. The
ChatGPT backend requires the OAuth token created by `oli login`.

## Prerequisites

- An Aperture instance reachable from the machine running Oli.
- Tailscale connected to the same tailnet as that instance.
- Admin access to configure Aperture providers and grants.
- A ChatGPT subscription supported by `oli login`.

## Configure Aperture

In the Aperture dashboard, open **Administration → Configuration** and add a
passthrough provider. Model names must exactly match models served by the
subscription; wildcards do not work.

```json
{
  "providers": {
    "codex-oauth": {
      "baseurl": "https://chatgpt.com/backend-api/codex",
      "authorization": "bearer",
      "auth_mode": "passthrough",
      "models": ["gpt-5.5"],
      "compatibility": {
        "openai_chat": true,
        "openai_responses": true
      }
    }
  }
}
```

Grant the intended Tailscale user or group access to every model it should be
able to use. Aperture is deny-by-default: a reachable instance without a model
grant still rejects inference requests.

Use the Aperture Models page to confirm the exact model ID. Replace `gpt-5.5`
in both configurations if the subscription serves a different model.

## Sign Oli in

Authenticate Oli directly with the ChatGPT subscription:

```bash
oli login
```

OAuth login and token refresh go directly to OpenAI. Aperture receives only
inference traffic, including the bearer credential it forwards unchanged to
the ChatGPT backend.

## Configure Oli

Add a provider to `~/.config/oli/config.toml`, or put the same block in a
project-scoped `.oli/config.toml`:

```toml
default_provider = "aperture"
default_model = "gpt-5.5"

[providers.aperture]
kind = "openai-chatgpt"
base_url = "http://<aperture-hostname>/codex"
default_model = "gpt-5.5"
```

Keep the `/codex` suffix. Oli appends `/responses`, so inference arrives at
Aperture on `/codex/responses`. Aperture removes the prefix before forwarding
the request to the ChatGPT subscription backend.

Use `http://`, not `https://`, for the tailnet endpoint unless the Aperture
instance explicitly says otherwise. WireGuard still encrypts traffic between
tailnet devices.

Do not add `api_key` or `api_key_env` to this provider. `openai-chatgpt` reads
the OAuth credentials stored by `oli login`.

## Verify the connection

Send one deterministic prompt:

```bash
oli -p "Respond with exactly: connected"
```

Then open Aperture's **Logs** page and confirm the request appears with the
expected Tailscale identity and model. Aperture observes model requests,
tokens, model-side tool calls, and estimated cost. Oli's local file, shell,
policy, and tool execution remains outside the gateway.

## Troubleshooting

### Model is available through `openai_responses`, not `openai_chat`

Oli is using `kind = "openai-compat"` against `/v1`. Subscription models use
the Responses API. Set `kind = "openai-chatgpt"` and use the `/codex` URL.

### Cloudflare HTML or `403 Forbidden`

The ChatGPT backend received a placeholder or otherwise invalid credential.
Confirm all of the following:

- Oli has completed `oli login`.
- The Oli provider kind is `openai-chatgpt`.
- The Oli base URL ends in `/codex`.
- The Aperture provider has `auth_mode` set to `passthrough` and no `apikey`.

### No route found for the model

The model is absent from the Aperture provider configuration or is not granted
to the current Tailscale identity. Copy an exact ID from Aperture's Models page
and update the provider, grant, and Oli configuration together.

### The Aperture hostname does not resolve

Check that Tailscale is running and connected to the correct tailnet:

```bash
tailscale status
```

The standard Aperture hostname is private and does not resolve through public
DNS.

## Security and capture boundary

Passthrough means the subscription bearer credential traverses Aperture so the
gateway can forward it upstream. Restrict Aperture administration and log
access, review its retention configuration, and verify the instance's header
redaction behavior before routing sensitive work.

Never commit OAuth tokens, provider keys, private tailnet hostnames, raw
response headers, or dashboard captures containing account identifiers.

For the upstream workflow, see Tailscale's official
[Codex subscription passthrough guide](https://tailscale.com/docs/aperture/how-to/use-passthrough-mode/codex-subscriptions).
