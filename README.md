# Arreo

> The agent runtime. Run them anywhere. Herd them from anywhere.

[![release](https://img.shields.io/github/v/release/arreo-dev/arreo?label=release)](https://github.com/arreo-dev/arreo/releases)
[![CI](https://img.shields.io/github/actions/workflow/status/arreo-dev/arreo/ci.yml?label=ci%20%28linux%20%C2%B7%20macos%20%C2%B7%20windows%29)](https://github.com/arreo-dev/arreo/actions)
[![crates.io](https://img.shields.io/crates/v/arreo-server)](https://crates.io/crates/arreo-server)
[![license: Apache-2.0](https://img.shields.io/badge/license%20core-Apache--2.0-blue)](#license)
[![license: AGPL-3.0](https://img.shields.io/badge/license%20relay-AGPL--3.0-red)](#license)
[![security audited](https://img.shields.io/badge/security-externally%20audited-brightgreen)](https://arreo.dev/security)

**Arreo is where your coding agents live — on a server you own, from any device you carry.**
Thirty agents across three machines, each in its own terminal, all running whether you're
watching or not. Your phone pings you when one is blocked on a question. You answer it from
the lock screen. Nothing stops, nothing breaks, and nothing — not even us — can read the traffic.

```console
curl -fsSL https://arreo.dev/install.sh | sh
arreo server init
arreo
```

- **Windows · Linux · macOS · iOS · Android · Web** — every platform first-class, one protocol.
- Free and open source: **Apache-2.0** for the runtime, **AGPL-3.0** for the relay.
- v1.0.0 · [Docs](https://arreo.dev/docs) · [Plugins](https://arreo.dev/plugins) · [Themes](https://arreo.dev/themes) · [Changelog](https://arreo.dev/changelog)

---

## Why

Coding agents are concurrent; humans are not. The tools that were supposed to fix this either
die when you close the terminal, trap your fleet inside a desktop app, or treat "remote" as an
afterthought bolted onto SSH config. And none of them will tell you what an agent is *doing*
— or that it's sitting blocked on a question while you're at lunch.

Arreo is a small Rust daemon that owns your agents' real terminals. Clients attach to it —
a TUI in your terminal, a CLI, a phone app, a browser tab, or *another Arreo server* — and
every one of them sees the same truth: live terminal state, per-agent memory and CPU,
and the exact moment an agent needs you.

| | **Arreo** | tmux / zellij | desktop ADEs | dashboards |
| --- | --- | --- | --- | --- |
| Work survives its UI closing | **yes — the daemon owns the PTYs** | detach | while the app runs | while it polls |
| Semantic agent state (`working / blocked / question / done`) | **yes, with payloads** | — | partial | process status |
| "Blocked on a question" notifications to your phone | **yes, from any harness** | — | partial | — |
| Multi-machine mesh, no SSH setup | **yes — pair with a code** | — | partial | — |
| Per-agent RAM/CPU meters + hard caps | **yes** | — | — | — |
| Updates that never kill a running agent | **yes** | no | no | — |
| 15-day offline server, zero-config reattach | **yes** | — | — | — |
| Native mobile + web, E2E encrypted | **yes** | — | companion app | browser only |
| Themes independent of the terminal | **yes** | no | n/a | n/a |
| Agent-native API (agents drive Arreo) | **yes** | terminal scripting | app APIs | workflow APIs |

## Quickstart

```console
# 1. Install (macOS, Linux, Windows — PowerShell: irm https://arreo.dev/install.ps1 | iex)
$ curl -fsSL https://arreo.dev/install.sh | sh

# 2. Start the daemon (installs a systemd user unit / launchd agent / Windows service)
$ arreo server init
✓ daemon running · 127.0.0.1 only · zero open ports

# 3. Work like you always do — or spawn agents directly
$ arreo spawn claude "refactor the auth module"
$ arreo                       # attach the TUI: 30 agents, states, RAM, one glance

# 4. Pair your phone (or another PC, or another server)
$ arreo pair
arreo: 7-gaze-iron-moon      # scan the QR or type the code on the other device
✓ workbox paired (ed25519 device cert pinned)
```

That's the entire onboarding. No SSH keys, no port forwarding, no config files.

## What's in the box

### Persistent by design

The daemon owns real PTYs. Close the lid, drop the network, reboot the machine — agents keep
running, and layout, scrollback, and state restore on restart. What survives is everything:
that's the point.

### It tells you when an agent is asking for you

Every harness blocks on questions; most tools can't tell you when. Arreo's state engine
detects it three ways, degrading honestly:

1. **Native** — harnesses with hooks or event streams ([CC], Codex, opencode, Pi, OMP, Gemini,
   Grok) hand us structured context: *"awaiting permission: `sudo apt install`"*, with
   allow/deny buttons in the notification.
2. **Universal** — for *any* CLI: output silence + cursor parked on a prompt-shaped line +
   bell → `question (inferred)`. Zero integration required, honestly labeled.
3. **Assisted** — `arreo agent state <id> question --text "…"` for anything exotic.

A `question` lights up everywhere at once — TUI sidebar, phone push with quick-reply,
web badge — and you answer it from wherever you are. It's just input to the PTY: audited,
E2E-encrypted, harness-agnostic.

### Machines, meshed

```console
$ arreo machines
workbox      VPS          · online · 12 agents · 2 blocked
raspberry    RPi5         · online · 8 agents  · 0 blocked
laptop       MacBook Pro  · offline · last seen 3 h ago
$ arreo attach workbox       # from any machine — including from another Arreo server
```

Servers are clients with PTYs attached: one protocol everywhere. Machine directory, presence,
and per-machine device trust, with zero inbound ports on any of them.

### Config, synced — edit once, replicate everywhere

```toml
# arreo.toml
[sync]
watch = ["opencode.jsonc", "~/.codex/config.toml", "projects/*/props.jsonc"]
```

Add a provider on the VPS and it reaches the Raspberry, the laptop, and the next machine you
pair — version vectors per file (Syncthing-grade), conflict copies with merge prompts, secrets
kept out (template variables resolve per machine from its own keychain), full revert history.

### Updates that never break your flow

`arreo update` stages a signed release, performs a **live handoff** — the new daemon inherits
every PTY, scrollback pointer, and session token from the old one — and clients reattach in
seconds. If the handoff fails for any reason, the old daemon keeps serving untouched.
Agents are never killed. Clients never stranded (N−1 protocol window).

### Resilience as a spec

Power the server off for two weeks. Turn it on. The relay — durable, end-to-end-encrypted,
free to self-host — queues everything in the meantime, and reattach is a fresh Noise-KK
handshake with pinned device keys that don't expire by age. Cursor-based sync drains the
queue, snapshot restores the view, and the agent that was `blocked` is still `blocked`.

### Themes that ignore the terminal

Themes emit explicit 24-bit color — the same theme looks identical in iTerm2, Alacritty,
Windows Terminal, or over SSH — with graceful fallback for legacy terminals and a `system`
theme for people who want Arreo to blend in. One JSON file drives the TUI *and* your phone.
Nine built-ins (`arreo`, tokyonight, catppuccin, gruvbox, nord, everforest, ayu, one-dark,
system), community gallery at [arreo.dev/themes](https://arreo.dev/themes).

### Plugins, sandboxed

```console
arreo plugin new token-meter   # ~30 lines to your first sidebar widget
cp token-meter.wasm ~/.config/arreo/plugins/   # hot-loaded, no restart
```

WASM components with capability-gated manifests: sidebar widgets, agent-card badges, custom
pane types, commands, keybindings, notification actions, theme tokens. A misbehaving plugin
gets throttled, never hangs the UI. Registry and signing at [arreo.dev/plugins](https://arreo.dev/plugins).

### Resource truth

Per-agent RAM and CPU, live, on every surface — and *enforced* where the OS allows it:
cgroups v2 on Linux, Job Objects on Windows, monitor+kill on macOS. An agent that blows its
budget gets throttled and you get told. The daemon itself holds **≤ 120 MB RSS at 30 panes
and 5 attached clients**; per-pane overhead ≤ 3 MB; older scrollback lives on disk.

### Agents drive Arreo too

The socket API is the same surface humans use — `read · send · wait · spawn · split · attach ·
metrics · machines` — plus an [agent skill](https://arreo.dev/docs/agent-skill) so your agents
can orchestrate each other: spawn panes, wait until a peer is genuinely blocked, collect results,
and report. Arreo is built by agents running inside Arreo; it doesn't get more dogfooded than that.

### Web, without installing anything

Open [app.arreo.dev](https://app.arreo.dev), pair with the same code/QR ritual (WebCrypto keys
stay in your browser), and every machine and agent is on the page — live terminal included.
WebTransport (HTTP/3) first, WebSocket fallback, end-to-end encrypted at the payload layer.

## Architecture

```text
┌──────────────────────────────────────────────────────────┐
│ your server                                              │
│  [agent claude] [agent codex] [agent pi] … (×30)          │
│        ↕ PTY (portable-pty)                               │
│  ┌────────────────────────────────────────────┐           │
│  │ arreo daemon (Rust, single binary)         │           │
│  │  · PTY/session manager + live handoff      │           │
│  │  · VT state per pane (alacritty_terminal)  │           │
│  │  · state engine (native → universal tier)  │           │
│  │  · metrics sampler (RAM/CPU per process)   │           │
│  │  · SQLite: sessions, metrics, audit, sync  │           │
│  └──────────────┬─────────────────────────────┘           │
└─────────────────┼─────────────────────────────────────────┘
                  │ outbound-only, Noise-encrypted QUIC
                  ▼
        ┌──────────────────────┐
        │ arreo relay (Rust)   │  self-hostable or managed
        │ · rendezvous/pairing │  (SPAKE2 — we can't read your traffic)
        │ · machine directory  │
        │ · durable inboxes    │
        │ · push (APNs/FCM)    │
        └──────┬────────┬──────┘
               ▼        ▼
        [TUI ratatui] [CLI] [phone: SwiftUI + Compose, Rust core via UniFFI]
        [web: WebTransport + WebCrypto, same Rust core as WASM]
        [plugins: WASM components, capability-gated]
```

## Security

Security is the product, not a feature list:

- **Zero inbound ports.** The daemon binds `127.0.0.1` only; all remote access is
  outbound QUIC to the relay (or LAN-direct when available).
- **Pairing you can audit.** SPAKE2 (RFC 9382) over a code you can say out loud — an active
  MITM gets exactly one guess. Every later connection is Noise-KK with pinned ed25519
  device certs. Revoke a stolen phone in one command.
- **The relay cannot read your traffic.** It routes ciphertext and stores durable inboxes as
  blobs with a TTL. This is an engineering constraint, not marketing.
- **Audit everything.** Every prompt sent, from which device, to which agent — append-only
  log, exportable.
- **Optional approval gates.** `sudo`, `rm -rf`, `git push` can require a human tap, from the
  lock screen if that's where you are.
- **Supply chain:** signed reproducible releases (cargo-dist + minisign), `cargo vet` in CI.
- **Audited before we charged a cent:** external security review, published
  [threat model](https://arreo.dev/security/threat-model).

## Performance

Enforced in CI, every release, on all three desktop platforms (`perf-budget.toml`):

| Metric | Budget |
| --- | --- |
| Daemon RSS @ 30 panes + 5 clients | ≤ 120 MB (agents excluded) |
| Per-pane harness overhead | ≤ 3 MB hot RAM |
| State detection latency | ≤ 200 ms |
| Phone cold attach (30 agents, 4G) | < 1 s |
| Cross-machine attach | < 3 s |
| Config sync propagation | < 10 s between online machines |
| Server reattach after 15 days offline | < 3 s, zero queued-message loss |
| Live server update handoff | agents never die; client reattach < 2 s |

## Pricing

Open core, freemium relay. Self-hosting is the whole product, free:

| Tier | Price | In short |
| --- | --- | --- |
| Self-host | Free forever | Everything, unlimited — run the AGPL relay yourself |
| Relay Free | Free, no card | Managed relay: 2 machines, 7-day inbox, push |
| Personal · $9/mo | | Unlimited machines, 30-day retention, config sync |
| Pro · $19/mo | | + web dashboard, 1-year metrics, approval gates |
| Team · $15/user/mo | | + roles, SSO, audit export, org machines |
| Enterprise | Custom | On-prem relay, SLA, security review |

Details at [arreo.dev/pricing](https://arreo.dev/pricing).

## Documentation

- [Getting started](https://arreo.dev/docs/quickstart)
- [Machines & mesh](https://arreo.dev/docs/machines)
- [Agent states & notifications](https://arreo.dev/docs/agents)
- [Config sync](https://arreo.dev/docs/sync)
- [Auto-updates](https://arreo.dev/docs/updates)
- [Theming](https://arreo.dev/docs/themes)
- [Plugins](https://arreo.dev/docs/plugins)
- [Socket API & agent skill](https://arreo.dev/docs/agent-skill)
- [Threat model](https://arreo.dev/security/threat-model)

## Contributing

We mean it — adapters, themes, plugins, docs, and code all have first-class paths in.
See [CONTRIBUTING.md](CONTRIBUTING.md). Fittingly for a tool that automates agents,
[the repo is developed in agent loops](ROADMAP.md) with human review as the gate.

## License

Arreo is dual-licensed:

| Component | License | Why |
| --- | --- | --- |
| Runtime: daemon, CLI, TUI, mobile core, plugin API | **[Apache-2.0](LICENSE)** | Max adoption, OSS goodwill |
| Relay (server + managed service) | **[AGPL-3.0](LICENSE-RELAY)** | The relay is the network — AGPL keeps a fork from productizing it as a SaaS without contributing back |

What this means in practice:

- **Users:** free forever, no strings, self-host anything — including the relay.
- **If you modify and distribute the relay**, you owe your recipients the AGPL source.
- **If you operate a *modified* relay as a network service**, AGPL §13 requires you to offer
  that modified source to your users. Running the *unmodified* upstream relay is fine.
- The core (Apache-2.0) has no such copyleft — build on it freely.

SPDX: `Apache-2.0` for `crates/*` except `crates/arreo-relay*` (`AGPL-3.0-or-later`); see
[REUSE.toml](REUSE.toml). Third-party notices: [NOTICE.md](NOTICE.md).

## Acknowledgments

Arreo stands on giants: [alacritty_terminal] and [portable-pty] for terminal truth,
[ratatui]/[crossterm] for the TUI, [quinn] for QUIC, [rust-noise] for handshakes, and the
[SPAKE2]/[magic-wormhole] lineage for pairing that humans can actually do. Herdr proved
agents want a daemon; Orca proved they want to be reachable from a pocket. We built both,
honestly about resources, in Rust.

[alacritty_terminal]: https://crates.io/crates/alacritty_terminal
[portable-pty]: https://crates.io/crates/portable-pty
[ratatui]: https://ratatui.rs
[crossterm]: https://crates.io/crates/crossterm
[quinn]: https://github.com/quinn-rs/quinn
[rust-noise]: https://github.com/iqlusioninc/crates/tree/main/noise-framework
[SPAKE2]: https://www.rfc-editor.org/rfc/rfc9382
[magic-wormhole]: https://magic-wormhole.readthedocs.io
