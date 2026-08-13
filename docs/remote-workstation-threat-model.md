# Remote workstation topology and threat model

This reference design runs oli and the source checkout on a private remote
workstation. A developer operates it from a laptop over a tailnet. The design
requires no public inbound port, reverse proxy, or Internet-facing SSH daemon.

## Topology

```mermaid
flowchart LR
    subgraph U[Developer trust boundary]
        B[Browser<br/>ChatGPT identity]
        L[Laptop<br/>user + tailnet device identity]
    end

    subgraph T[Tailnet reachability boundary]
        W[Remote workstation<br/>Linux user + tailnet device identity]
        O[oli process<br/>provider and tool policy]
        R[Source checkout]
        S[Session store<br/>~/.config/oli/sessions]
        C[Credential store<br/>~/.config/oli/auth.json]
        W --> O
        O <--> R
        O <--> S
        O <--> C
    end

    subgraph P[Provider trust boundary]
        M[Provider / model API]
    end

    L -->|Tailscale SSH, or SSH over WireGuard/TCP 22| W
    O -.->|HTTPS provider protocol (remote providers only)| M
    O -->|OAuth authorization URL| B
    B -->|pasted localhost redirect URL or device approval| O
```

The browser and oli may run on different hosts. `oli login --paste` carries the
OAuth redirect URL back through the existing terminal session; device auth
carries only a short-lived user code. The browser never connects inbound to the
workstation. Remote provider traffic is outbound HTTPS from the workstation.
Local providers (e.g., `http://localhost:11434/v1` for Ollama) remain confined
to the workstation and do not cross a network boundary; they are excluded from
the "Provider trust boundary" shown above.

## Identities and protocols

| Hop or store | Identity | Protocol | Authorization authority |
|---|---|---|---|
| Laptop to tailnet | User and enrolled device | WireGuard via Tailscale | Tailnet device enrollment and policy |
| Laptop to workstation | Tailnet identity plus remote Unix user | Tailscale SSH, or ordinary SSH over tailnet TCP | Tailnet SSH policy, or host `sshd` configuration |
| oli to provider | Provider account, API key, or subscription token | HTTPS provider API | Provider |
| Browser login | Provider user | OAuth authorization-code/PKCE or device flow | Provider |
| oli session data | Remote Unix user | Local filesystem | Workstation permissions |

oli does not integrate with Tailscale directly. Tailscale supplies private
reachability, device identity, and network policy; oli continues to use ordinary
SSH, HTTPS, OAuth, and terminal protocols.

## SSH choice

The reference setup must choose and record one of these modes:

- **Tailscale SSH** lets Tailscale handle the SSH connection and authorize the
  tailnet source identity, destination, and requested Unix user through tailnet
  SSH policy. It does not mean every tailnet member may log in; the SSH rules
  must grant that separately from network reachability.
- **Ordinary SSH over a tailnet** sends standard SSH to the workstation's
  `sshd` over its tailnet address or MagicDNS name. Tailnet policy controls who
  can reach TCP 22, while host SSH keys, certificates, PAM, and `sshd_config`
  still authenticate and authorize the Unix login.

Do not mix the models accidentally. An access grant for TCP 22 is not a
Tailscale SSH login rule, and a Tailscale SSH rule is not an `authorized_keys`
entry. PA-102 should test the selected mode and one denied identity.

## Required access policy

- Only the developer identity and explicitly managed developer devices may
  initiate SSH to the workstation.
- The workstation must not expose SSH on a public interface. Host firewall and
  cloud security-group rules deny public ingress even if `sshd` listens beyond
  the tailnet interface.
- The remote login maps to a non-root Unix account. Privilege escalation is a
  separate, explicit host policy.
- Tailnet policy is default-deny for the workstation and grants only the chosen
  SSH mode. Provider HTTPS remains outbound traffic.
- Device approval, key expiry, and identity-provider account controls are
  enabled according to the tailnet's risk level.
- oli tool policy remains independent. Network authorization to the workstation
  does not authorize every command oli can execute after login.

## Assets and trust assumptions

Protected assets are the source checkout, uncommitted changes, provider
credentials, oli configuration, session transcripts, notes, policy decisions,
SSH credentials, and the workstation account itself.

The design assumes the laptop terminal, remote workstation OS, tailnet control
plane, identity provider, and selected model provider are trusted for the data
they necessarily process. It does not claim end-to-end secrecy from a
compromised endpoint or provider. Tailnet encryption protects traffic between
devices; it does not encrypt data at rest or constrain a process after login.

## Threats and responses

| Threat | Consequence | Required response or control |
|---|---|---|
| Lost or stolen laptop | An active device session may reach the workstation | Revoke the tailnet device immediately, invalidate identity-provider sessions, and rotate laptop-held authentication material (tailnet/device identity and SSH keys per the chosen SSH mode); require local disk encryption and screen lock |
| Compromised laptop | Attacker acts as the developer and can copy terminal output | Revoke the device and user sessions, inspect workstation auth logs and oli sessions, rotate credentials, and rebuild the laptop before reenrollment |
| Compromised workstation | Attacker reads source, transcripts, config, and provider tokens and can alter oli | Isolate and revoke the node, rotate every credential present, preserve evidence, rebuild from a trusted image, and restore only reviewed source; tailnet policy cannot contain an attacker already on the host |
| Stolen provider credential | Attacker consumes provider access outside the tailnet | Provider credentials and `~/.config/oli/auth.json` remain only on the workstation (never on the laptop) with mode `0600`; use environment or secret-manager injection for API keys where practical, revoke at the provider, and review usage |
| Leaked pasted redirect URL | OAuth code may be exchanged before expiry | Paste only into the intended oli process, never logs or chat, complete promptly, and restart login if exposed; PKCE limits use without the workstation's verifier |
| Overbroad tailnet policy | Unintended identities can attempt workstation login | Use a dedicated workstation tag or group, default deny, review policy changes, and test both allowed and denied identities |
| Public SSH exposure | Internet attackers bypass the intended private-reachability boundary | Remove public addresses where possible and deny inbound TCP 22 in host and cloud firewalls; verify from a non-tailnet network |
| Plaintext session storage | Anyone with the Unix account or disk access reads conversation history | Restrict account and filesystem access, encrypt the workstation disk, set retention expectations, and delete sessions deliberately when no longer needed |
| Malicious model or tool output | oli may be induced to request harmful tool actions | Keep oli policy appropriate to the repository and host; tailnet policy is not a substitute for tool authorization |

## Security invariants for PA-102

The implementation example is acceptable only when all of these hold:

1. A clean laptop reaches the workstation by its tailnet name without public
   ingress.
2. An unauthorized tailnet identity and a non-tailnet Internet host cannot
   establish the selected SSH path.
3. Browser-localhost confusion is avoided with `oli login --paste` or device
   auth, and credentials are stored only on the workstation.
4. Disconnecting the terminal does not move source or session state to the
   laptop; `--resume` continues the remote session.
5. The chosen provider and its data boundary are documented. A private
   workstation does not make a third-party model provider private.
