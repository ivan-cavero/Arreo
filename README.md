# Arreo

> The agent runtime. Run them anywhere. Herd them from anywhere.

> **Status: pre-launch and pre-1.0.** There is no release, no published crate, no
> installer and no `arreo.dev` — the domain does not resolve. The only way to get
> Arreo today is to build this checkout. Everything below was run against this
> tree; anything that is designed but not built is named in
> [What is not here yet](#what-is-not-here-yet) rather than described as a feature.
> New here? [docs/tour.md](docs/tour.md) explains how the pieces fit without the
> rest of this file.

**Arreo is a daemon that owns your coding agents' real terminals.** The agents keep
running whether or not you are looking: your terminal, a script, or another machine
attaches to the daemon and sees the same panes, the same scrollback, the same live
memory numbers, and the same answer to "is this agent working, blocked, or waiting
on me?".

Clients: `arreo` (CLI), `arreo-tui` (ratatui terminal UI). Machines reach each other
through a self-hostable relay. The runtime is Apache-2.0; the relay is
AGPL-3.0-or-later.

## Why a daemon

Coding agents are concurrent; humans are not. The tools around them tend to die with
the window you closed, trap the fleet inside a desktop app, or treat "remote" as an
afterthought bolted onto SSH config. And most of them will not tell you that an agent
is sitting on a question while you are at lunch.

| Property | Today |
| --- | --- |
| Work survives its UI closing | Yes — the daemon owns the PTYs, not the client. |
| Work survives the daemon crashing | Yes — panes and scrollback restore from the sidecar store on restart. |
| Semantic agent state | `working / idle / question / blocked / done`, with the pattern or event that decided it, and an honest `inferred` vs `direct` label. |
| Per-agent resources | Live RSS/CPU per pane tree, plus a durable metrics history. |
| One surface for humans, scripts and agents | The same socket verbs; see [docs/agent-skill.md](docs/agent-skill.md). |
| More than one machine | A relay, a machine directory, and a pairing ritual that needs no IP addresses or SSH. |
| An audit trail | Append-only, secrets redacted at write time; see [docs/audit.md](docs/audit.md). |

## Build

```console
git clone https://github.com/ivan-cavero/Arreo && cd Arreo
cargo build --workspace
```

Requires stable Rust, pinned by `rust-toolchain.toml`. Building from source *is* the
install — there is no package, no tap and no installer. Linux, macOS and Windows are
build targets, and `.github/workflows/ci.yml` is written to build, test (with the
supply-chain and portability gates) and run the e2e slices on all three. **That workflow
is currently red on all three legs**, so treat "it builds on Windows/macOS" as unproven
until CI is green — [docs/cross-os.md](docs/cross-os.md) is explicit about which claims
rest on which layer. Binaries land in `target/debug/`: `arreo-server` (the daemon),
`arreo` (the CLI), `arreo-tui` (the TUI), `arreo-relay` (the relay; AGPL).

## First pane in 60 seconds

```console
# 1. The daemon owns the terminals, so it runs first. With no --socket it uses
#    $XDG_RUNTIME_DIR/arreo.sock (else /tmp/arreo-<uid>.sock).
$ ./target/debug/arreo-server &
arreo-server: device authority ready (root c958f66db31c0e20…, 0 device(s))
arreo-server: serving on /run/user/1000/arreo.sock

# 2. Spawn a pane: `spawn <id> <program> [args...]`. Any program the shell can
#    run is a valid agent — Arreo does not need to know it.
$ ./target/debug/arreo spawn build /bin/sh
spawned build

# 3. Look at it, or attach the TUI.
$ ./target/debug/arreo read build
$ ./target/debug/arreo-tui      # j/k move · Enter attach · w wall · t theme · q quit

# 4. Type into it — `send` writes bytes, so include the carriage return to press
#    Enter — then block until the pane settles instead of polling.
$ ./target/debug/arreo send build $'echo hello-arreo\r'
$ ./target/debug/arreo wait build --state idle --timeout 30s
state=Idle confidence=inferred:silence pattern=None

# 5. Stop the daemon gracefully: it drains, then exits 0. Panes and scrollback
#    come back on the next start.
$ ./target/debug/arreo server stop
server stopped (pid 12345)
```

For a real agent, the wait that matters is `arreo wait <id> --state question
--timeout 5m`: it returns the moment the harness asks something, with the pattern or
event that decided it, instead of making you poll. `arreo attach <id>` streams a pane's
output to stdout instead of taking over the terminal; Ctrl-C detaches and the pane keeps
running. Every verb that talks to a daemon takes `--socket PATH` (after the verb) to
reach a daemon other than the default one; the purely local verbs (`record`, `replay`,
`metrics --pid`) do not.

To have the OS manage the daemon instead of starting it by hand, `arreo service
install` writes the unit for this machine — a systemd user unit, a launchd agent,
or a `sc.exe` script on Windows — and enables it where the platform allows.

## What the daemon can do today

```console
arreo --version                              # the build you are running
arreo panes                                  # every pane: id, state, alert
arreo spawn <id> <program> [args...]         # start a pane
arreo read <id> [--from N]                   # one-shot snapshot of a pane
arreo attach <id>                            # stream a pane until it exits
arreo send <id> <text...>                    # type into a pane
arreo wait <id> --state <s> [--timeout 5m]   # block until a state, exit 0/1
arreo split <id> <new-id>                    # sibling pane, same program
arreo metrics <id>                           # RSS/CPU/pids for one pane's tree
arreo metrics history <pane> --since 6h --step 1m
arreo audit [--limit N] [--json]             # the append-only log, read directly
arreo audit export|prune …
arreo devices id|list|issue|rotate|revoke|authorize
arreo pair [--role owner|viewer]             # four words (300 s default); pins the device that types them
arreo pair --join "four words" --uri …       # the joining side of that exchange
arreo machines list|status|rename|remove|add|trust
arreo attach --machine <name> [<pane>]       # another machine's pane, by name
arreo service install|uninstall|status
arreo server stop                            # graceful drain + exit 0
```

The same verbs work against a daemon on another machine: `panes`, `read`, `send`,
`wait`, `split`, `metrics` and `attach` all take `--machine <name>`, and
`arreo-tui --machine <name>` opens that machine's panes. The name resolves through
the account's directory, so nothing is dialed from your command line.

States are `working|idle|question|blocked|done|unknown`; durations are `500ms`,
`30s`, `5m`. The detection rule is one engine over one adapter: the daemon runs the
embedded `adapters/default.toml` (question/error regexes plus silence thresholds and
the bell), and per-harness shapes for `opencode` and `pi` are recorded in the repo as
the next step once adapter selection is wired. `arreo wait --state question` is the
primitive an agent uses instead of polling.

Themes are JSON, embedded (five ship in the binary: `arreo`, `tokyonight`,
`catppuccin`, `gruvbox`, `system`) and discovered from `$XDG_CONFIG_HOME/arreo/themes`
and `./.arreo/themes`. They emit explicit 24-bit colour with an honest depth fallback,
so the TUI picks the closest thing a legacy terminal can render rather than glitching.

## What is not here yet

Named plainly, because a roadmap item is not a feature:

- **No releases, no downloads, no published artifacts.** No installer, no Homebrew
  tap, no crate on crates.io, no signed binary. [docs/release.md](docs/release.md)
  records the reserved URLs and says they are reserved.
- **No mobile apps and no web client.** The phone and browser surfaces are roadmap
  work; today the clients are the CLI and the TUI.
- **No config sync.** Agreeing on a file across machines is unbuilt.
- **No auto-update and no live handoff.** Upgrading means building and restarting.
- **No plugin runtime.** `arreo-plugin-api` is an empty shell so the license boundary
  and the workspace shape are settled; nothing loads plugins.
- **No push notifications and no approval gates.** A `question` is visible to
  anything attached to the daemon; nothing rings your phone.
- **No hosted service, no pricing, no account system.** The relay is the only network
  component, and you run it.
- **Resource enforcement is Linux-only.** Per-pane cgroup v2 budgets
  (`memory.max` + `pids.max`) work on Linux; Windows Job Objects and the macOS
  monitor path return `Unimplemented` rather than pretending.

## How the relay fits

The daemon binds a Unix socket and opens no network port. When a machine needs to be
reachable by another, both sides dial **out** to a relay over QUIC, and the relay
routes bytes it cannot read.

```text
arreo-server ──outbound QUIC──▶ arreo-relay ◀──outbound QUIC── arreo-server
 (machine A, socket API)         (AGPL-3.0)                     (machine B)
                                  · pairing mailbox (SPAKE2)
                                  · machine directory
                                  · durable per-device inbox
```

Pairing is a four-word code shown on the machine that already belongs; the device
being added proves it saw that screen through SPAKE2, and receives a certificate
signed by the account root. Later connections authenticate the certificate, and each
machine keeps its **own** trust ledger deciding which devices it will serve — so a
phone paired to one machine is not automatically trusted by another.
[docs/machines.md](docs/machines.md) is the reference; [docs/relay-deploy.md](docs/relay-deploy.md)
is how to run the relay; [docs/relay-protocol.md](docs/relay-protocol.md) is the wire.

The license boundary is a design constraint, not paperwork. `arreo-relay` may depend
on Apache-licensed first-party crates, but **no Apache-licensed crate may depend on
the relay** — otherwise the shipped binary would become AGPL. `cargo test -p xtask
--test workspace_deps` fails the build if that direction is ever reversed, and
`cargo xtask release-check` re-reads the declared license of every crate. The relay's
protocol vocabulary deliberately lives in Apache `arreo-core` so a third party can
implement a relay or a client from [the spec](docs/relay-protocol.md) without linking
AGPL code.

## Security, as it stands

- **No inbound port.** The daemon listens on a Unix socket. Remote access is
  outbound QUIC to a relay you run.
- **Pairing you can audit.** SPAKE2 (RFC 9382) over four words — an active
  MITM gets one guess and a wrong guess ends the session.
- **The relay cannot read the traffic.** It routes opaque envelopes and stores a
  durable per-device inbox. That is an engineering constraint, not a slogan.
- **Devices are revocable, per machine.** `arreo devices revoke` ends a device's
  certificate everywhere; `arreo devices revoke --machine <name>` and `arreo
  machines trust` change one machine's grant. Both write audit rows.
- **The audit trail is append-only by API.** The store has no update and no delete
  for it; the only removal path is an explicit, bounded `arreo audit prune`. It is
  *not* tamper-evident — anyone who can write the SQLite file can edit it, and
  [docs/audit.md](docs/audit.md) says so rather than implying a hash chain exists.
- **Nothing is signed yet.** [docs/release.md](docs/release.md) states that release
  artifacts are unsigned until signing lands, and forbids presenting them otherwise.
- **No external security review has happened**, and [SECURITY.md](SECURITY.md) says
  so itself: it records that the private reporting channel is not enabled on this
  repository yet and that the report path is the fallback it describes. The threat
  model — and what it deliberately does *not* defend against — is `ROADMAP.md` §4.

## Performance

`perf-budget.toml` is the law, and `cargo xtask bench` is the judge: it spawns real
panes, replays recorded fixtures through the real engine and sampler, and fails the
command on a regression. Rows marked `phase0 = true` are asserted today:

| Metric | Budget |
| --- | --- |
| Daemon RSS, 10 panes | ≤ 100 MB |
| Per-pane harness overhead (hot RAM) | ≤ 3 MB |
| State detection latency after output | ≤ 200 ms |
| Sampler sweep, 30 panes | ≤ 300 ms |
| Single-byte VT feed | ≤ 50 ms |
| Fixture replay determinism | byte-exact, 0 mismatches |
| Release daemon binary | ≤ 20 MB |

The rows that need a phone, a relay, a second machine or a live update are recorded
in the same file with `phase0 = false`: they are the target, and `bench` skips them
by name rather than quietly passing them. `cargo xtask bench [--json]` runs the whole
check locally, against the same file. The nightly workflow
(`.github/workflows/nightly-bench.yml`) is written to run that bench on Linux, macOS and
Windows and upload the JSON as an artifact, but **it has not succeeded yet** — the one
recorded run failed — so there is no artifact to point at. The stable PR-time check is
`cargo xtask bench --probe size` (the release binary size row), which CI runs.

The 30-pane, ≤ 120 MB row is recorded and **not** enforced yet — the Phase 0 proof
point is 10 panes.

## Documentation

Everything is in this repository; no external docs site exists yet.

| Read | For |
| --- | --- |
| [docs/tour.md](docs/tour.md) | How the pieces fit, where state lives, what each `cargo xtask` command does |
| [AGENTS.md](AGENTS.md) | The build, test and evidence commands for this repo |
| [ROADMAP.md](ROADMAP.md) | The product's design and its phases (§6 phases, §7 licensing) |
| [docs/audit.md](docs/audit.md) | What the audit log records, redacts, and refuses to record |
| [docs/machines.md](docs/machines.md) | The machine directory, presence, per-machine trust |
| [docs/relay-protocol.md](docs/relay-protocol.md) | The relay wire protocol, version 1 (normative) |
| [docs/relay-deploy.md](docs/relay-deploy.md) | Running your own relay |
| [docs/cross-os.md](docs/cross-os.md) | What each portability layer proves — and what it does not |
| [docs/conpty-windows.md](docs/conpty-windows.md) | What is actually known about Windows/ConPTY |
| [docs/agent-skill.md](docs/agent-skill.md) | Driving Arreo from inside a coding agent |
| [docs/release.md](docs/release.md) | How a release is built, signed, and verified |
| [specs/adr/](specs/adr/) | One ADR per design decision, with the alternatives rejected |

## Contributing

Adapters, themes, docs and code all have a path in. Read
[CONTRIBUTING.md](CONTRIBUTING.md) first — strict TDD, the e2e battery as the
definition of "works", executable perf budgets, and a DCO sign-off on every commit.
For a tool that automates agents it is fittingly dogfooded: [the repo is developed in
agent loops](ROADMAP.md#10-building-arreo-with-agents-the-repo-is-developed-in-agent-loops)
with human review as the merge gate.

## License

Arreo is dual-licensed.

| Component | License | Why |
| --- | --- | --- |
| Runtime: daemon, CLI, TUI, core, plugin API | **[Apache-2.0](LICENSE)** | Maximum adoption. |
| Relay (`crates/arreo-relay*`) | **[AGPL-3.0](LICENSE-RELAY)** | The relay is the network; AGPL keeps a fork from productizing it without contributing back. |

What that means in practice:

- **Users:** free to run, self-host anything including the relay.
- **If you modify and distribute the relay**, you owe your recipients the AGPL source.
- **If you operate a *modified* relay as a network service**, AGPL §13 requires you
  to offer that modified source to your users. Running the unmodified upstream relay
  is fine.
- **The Apache core has no such copyleft** — build on it freely. Nothing in it may
  depend on the relay.

SPDX: `Apache-2.0` for `crates/*` except `crates/arreo-relay*`
(`AGPL-3.0-or-later`); see [REUSE.toml](REUSE.toml). Third-party notices and
trademark terms: [NOTICE.md](NOTICE.md).

## Acknowledgments

Arreo stands on giants: [alacritty_terminal] and [portable-pty] for terminal truth,
[ratatui]/[crossterm] for the TUI, [quinn] for QUIC, [snow] for the Noise-KK
handshake, and the [SPAKE2]/[magic-wormhole] lineage for pairing that humans can
actually do. Herdr proved agents want a daemon; Orca proved they want to be reachable
from a pocket. We built both, honestly about resources, in Rust.

[alacritty_terminal]: https://crates.io/crates/alacritty_terminal
[portable-pty]: https://crates.io/crates/portable-pty
[ratatui]: https://ratatui.rs
[crossterm]: https://crates.io/crates/crossterm
[quinn]: https://github.com/quinn-rs/quinn
[snow]: https://github.com/mcginty/snow
[SPAKE2]: https://www.rfc-editor.org/rfc/rfc9382
[magic-wormhole]: https://magic-wormhole.readthedocs.io
