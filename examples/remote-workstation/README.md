# Reproducible remote oli workstation

This example implements the [remote-workstation threat model](../../docs/remote-workstation-threat-model.md)
using ordinary OpenSSH over a tailnet. Tailscale controls reachability to TCP
22; the workstation's `sshd` and SSH keys authenticate the Unix login. It does
not use Tailscale SSH.

The commands target a clean Ubuntu 24.04 workstation and a laptop already
enrolled in the same tailnet. Replace `developer@example.com` and
`unlisted@example.com` with real allowed and denied tailnet identities, then
replace the workstation tag, Unix username, and MagicDNS name for the real
environment.

## 1. Tailnet policy

Copy `tailnet-policy.hujson` into the tailnet policy editor, replace both example
identities, and save only after its embedded positive and negative tests pass.
Assign `tag:oli-workstation` to the workstation during enrollment. Keep the
example's `ssh` section absent: this setup uses the host SSH daemon, not
Tailscale SSH.

On the clean workstation console:

```console
sudo apt-get update
sudo apt-get install --yes build-essential curl git openssh-server
sudo systemctl enable --now ssh
curl -fsSL https://tailscale.com/install.sh | sh
sudo tailscale up --auth-key="$TAILSCALE_AUTH_KEY" --advertise-tags=tag:oli-workstation --ssh=false
tailscale status
```

Use a short-lived, preauthorized, tagged auth key supplied through the console;
do not save it in shell history or the repository. Remove it from the shell
after enrollment:

```console
unset TAILSCALE_AUTH_KEY
```

Cloud security groups and the host firewall must deny public TCP 22. The policy
grant permits only the developer group to the tagged node over the tailnet.
Verify both a non-tailnet public path and an unlisted tailnet identity fail;
record sanitized results in `verification.md`.

From the enrolled laptop, verify the selected ordinary-SSH path:

```console
ssh-keygen -t ed25519 -a 64 -f ~/.ssh/oli-workstation
ssh-copy-id -i ~/.ssh/oli-workstation.pub developer@oli-workstation
ssh -i ~/.ssh/oli-workstation developer@oli-workstation
```

`ssh-copy-id` requires a temporary host-approved bootstrap login. Disable that
bootstrap path after the key is installed. A managed SSH certificate or image-
provisioned key is preferable where available.

## 2. Install the pinned oli release

Run on the workstation as the non-root developer account:

```console
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain 1.95.0
. "$HOME/.cargo/env"
git clone --branch v0.1.0 --depth 1 https://github.com/sudoish/oli.git
cd oli
cargo install --locked --path .
oli --version
```

The `v0.1.0` tag is the minimum baseline. Use a later reviewed release by
changing the tag explicitly; never use moving `master` in a reproducible run.

## 3. Choose one provider

All configuration and credentials live on the workstation, not the laptop.

### ChatGPT subscription

```console
mkdir -p ~/.config/oli
cp examples/remote-workstation/chatgpt.toml ~/.config/oli/config.toml
oli login --paste
oli login --check
```

Open the printed URL in the laptop browser. Its localhost redirect will fail to
connect on the laptop by design; paste the complete address-bar URL into the
remote oli prompt. Never paste it into chat, logs, or the shell command line.
Use `oli login --device-auth` instead when copying a redirect is unsuitable.

### Hosted API-key provider

```console
mkdir -p ~/.config/oli
cp examples/remote-workstation/openrouter.toml ~/.config/oli/config.toml
read -rsp 'OpenRouter API key: ' OPENROUTER_API_KEY; echo
export OPENROUTER_API_KEY
oli run -p 'Reply with exactly OLI_REMOTE_OK'
unset OPENROUTER_API_KEY
```

For regular use, inject `OPENROUTER_API_KEY` from the workstation's secret
manager or login environment. Do not write it into this example or shell
history.

### Local/private model

Install Ollama through its reviewed package for the workstation, then:

```console
ollama pull qwen3-coder:30b
mkdir -p ~/.config/oli
cp examples/remote-workstation/ollama.toml ~/.config/oli/config.toml
oli run -p 'Reply with exactly OLI_REMOTE_OK'
```

The example binds oli to loopback Ollama. A model on another private GPU node is
the separate private-model-plane workflow and needs its own network policy.

## 4. Disconnect and resume

Create a persisted conversation through SSH:

```console
cd ~/oli
oli run -p 'Reply with exactly OLI_REMOTE_SESSION_OK'
```

Record the conversation id printed on stderr outside the repository. Close the
SSH terminal, reconnect, and append a turn to the same conversation:

```console
ssh -i ~/.ssh/oli-workstation developer@oli-workstation
cd ~/oli
oli run --conversation <conversation-id> -p 'Summarize our previous turn in one sentence'
```

Confirm the command returns context from the first turn and prints the same
conversation id. Use `--output json` when a remote automation needs to capture
the id and response without parsing terminal prose.

## 5. Exercise and record the run

Copy `verification.md` outside the checkout, fill every common row and every row
for the selected provider, and retain it with the release evidence. Mark rows
for unselected providers `N/A (provider not selected)`; do not use N/A for a
required check. The example is not verified merely because configuration parses
or an authorized login succeeds: the denied identity, public-path denial,
remote login, selected provider, conversation persistence, interrupted session,
and resume must all be exercised.

## Troubleshooting

| Symptom | Check | Recovery |
|---|---|---|
| Browser waits on `localhost` | Plain `oli login` was used on the workstation | Cancel and use `oli login --paste` or `--device-auth`; do not forward a public callback port |
| Pasted redirect is rejected | URL is incomplete, expired, or belongs to another login attempt | Start `oli login --paste` again and paste the complete fresh address-bar URL into that same process |
| Device code never completes | Code expired, wrong browser account, or provider endpoint changed | Restart `--device-auth`; if the endpoint is unavailable, use an API-key provider |
| `oli login --check` fails refresh or models | Credentials were revoked, plan access changed, or the undocumented backend moved | Run `oli login` again; if it persists, switch to the documented API-key path |
| Workstation MagicDNS name does not resolve | Laptop is disconnected, MagicDNS is disabled, or split DNS conflicts | Check `tailscale status`, tailnet DNS settings, and the workstation tailnet IP |
| SSH times out | Tailnet ACL, cloud firewall, host firewall, or `sshd` is blocking the path | Test each boundary in that order; do not open public TCP 22 as a shortcut |
| SSH says permission denied | Network path works but host SSH authentication failed | Check the intended Unix user, public key, file permissions, and `sshd` logs |
| Conversation id is missing | Output was redirected without stderr | Capture stderr in text mode, or use `oli run --output json` |
| Resume cannot find a session | Wrong remote Unix user or config directory | Run `/paths` and `/sessions` as the same workstation account that created it |
| Credential file has loose permissions | File was copied or restored incorrectly | Remove it and sign in again; confirm `~/.config/oli/auth.json` is mode `0600` |
