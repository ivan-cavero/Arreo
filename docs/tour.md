# A tour of Arreo

> Read this without the roadmap. It assumes you have the repository checked out,
> a Rust toolchain, and no idea what any of the crates do.

**One sentence: a daemon owns your agents' real terminals, and everything else is
a client of that daemon.**

That single decision is what the rest of the product falls out of. The terminals
are not owned by the window you happen to have open, so closing the window does
not kill the work. The state of a terminal is not inferred by the client, so every
client sees the same thing. And a client that is not on this machine at all is a
normal case rather than a bolt-on, because the only thing the client needs is a
connection to the daemon.

---

## The pieces

```text
      your machine                                          another machine
┌──────────────────────────────────────────────┐    ┌───────────────────────────┐
│  arreo-server   — the daemon                 │    │  arreo-server             │
│   owns the PTYs, the VT state per pane,      │    │   (the same binary)       │
│   the state engine, the metrics sampler      │    └─────────────┬─────────────┘
│                                              │                  │
│      ▲  framed MessagePack over the Unix     │                  │
│      │  socket the daemon owns:              │                  │
│      │  $XDG_RUNTIME_DIR/arreo.sock          │                  │
│                                              │                  │
│   ┌──┴───────┬───────────┐                   │                  │
│   │ arreo    │ arreo-tui │                   │                  │
│   │ (CLI)    │ (TUI)     │                   │                  │
│   └──────────┴───────────┘                   │                  │
└──────────────────────────────────────────────┘                  │
                    │                                             │
                    │  outbound-only QUIC, Noise-encrypted        │
                    ▼                                             ▼
      ┌───────────────────────────────────────────────────────────────┐
      │  arreo-relay  (AGPL-3.0-or-later, self-hostable)              │
      │   · pairing mailbox — SPAKE2 over a four-word code            │
      │   · machine directory — names, ids, presence                  │
      │   · durable per-device inbox — routes ciphertext it cannot    │
      │     read, and keeps it while a device is away                 │
      └───────────────────────────────────────────────────────────────┘
```

- **`arreo-server`** is the daemon. It spawns programs in real PTYs, keeps a
  terminal emulator's worth of state for each one, samples their memory and CPU,
  and serves the socket API. Nothing in this repo works without it.
- **`arreo`** is the CLI. Every verb is one request to the daemon (or, with
  `--machine`, to a daemon on another machine through the relay). It is the
  scripting and agent surface.
- **`arreo-tui`** is the ratatui client: a sidebar of panes with their state and
  RAM, and a pane wall. It is a separate binary on purpose — the daemon does not
  have a UI, so the UI can be closed and reopened freely. It is themed from the
  brand palette in [`design/BRAND.md`](../design/BRAND.md), and every state is a
  dot *shape* and its own name, so the board is readable with no color at all.
- **`arreo-relay`** is the network component. It is the only piece that needs to be
  reachable from the internet, and it is the only AGPL-licensed crate. It routes
  bytes it cannot decrypt; it also holds the machine directory and the pairing
  mailbox.
- **The machine directory and pairing are how machines find each other.** You do
  not configure IP addresses or SSH. An account's root key is its identity: the
  relay accepts a device certificate only if it verifies under the key the account
  was registered with. So the **first** machine makes that key, you register it at
  your relay, and the machine admits itself (`arreo pair`, then `arreo pair --join`
  with its own code). After that, adding a machine is one code: `arreo pair` on a
  machine that already belongs prints four words, and `arreo machines add` on the
  machine being added takes them, does a SPAKE2 exchange, receives a device
  certificate, and registers itself in the directory. Every later connection is
  authenticated by that certificate. Run the relay yourself or don't run one at
  all — a single machine needs no relay.

  The first-machine steps, with the exact commands and which half of the key to
  paste where, are in [machines.md](machines.md#the-first-machine).

## First pane in 60 seconds

Pre-launch: there is no installer and no published binary. Build the workspace
(`cargo build --workspace` — the binaries land in `target/debug/`), then run this
against the binaries you just built. Every line below was executed verbatim on a
Linux dev box; the one platform-specific note is that `--socket` is required on
Windows/macOS the same way it is here.

```console
cargo build --workspace

# 1. The daemon owns the terminals, so it runs first. With no --socket it uses
#    $XDG_RUNTIME_DIR/arreo.sock (else /tmp/arreo-<uid>.sock).
./target/debug/arreo-server &
arreo-server: device authority ready (root c958f66db31c0e20…, 0 device(s))
arreo-server: serving on /run/user/1000/arreo.sock

# 2. Spawn a pane: `spawn <id> <program> [args...]`. Any program the shell can
#    run is a valid agent — Arreo does not need to know it.
./target/debug/arreo spawn build /bin/sh
spawned build

# 3. Look at it, or attach the TUI.
./target/debug/arreo read build
./target/debug/arreo-tui      # j/k move · Enter attach · w wall · t theme · ? keys · q quit

# 4. Type into it — `send` writes bytes, so include the carriage return to press
#    Enter — then block until it needs a human.
./target/debug/arreo send build $'echo hello-arreo\r'
./target/debug/arreo wait build --state idle --timeout 30s
state=Idle confidence=inferred:silence pattern=None

# 5. Stop the daemon gracefully: it drains, then exits 0. Panes and scrollback
#    come back on the next start.
./target/debug/arreo server stop
server stopped (pid 12345)
```

For a real agent, the wait that matters is `arreo wait <id> --state question
--timeout 5m`: it returns the moment the harness asks something, with the pattern or
event that decided it, instead of making you poll. `arreo attach <id>` streams a pane's
output to stdout instead of taking over the terminal; Ctrl-C detaches and the pane keeps
running. If the daemon is handed over underneath a live attach (`update --server`), the
client reconnects on its own and the stream resumes at exactly the line where it stopped —
the transcript is continuous, never a replay. Every verb that talks to a daemon takes
`--socket PATH` (after the verb) to reach a daemon other than the default one; the purely
local verbs (`record`, `replay`, `metrics --pid`) do not.

In the TUI: `j`/`k` move, `Enter` attaches, `w` toggles the pane wall, `t` opens
the theme picker, `/` searches, `?` lists every key on screen, `q` quits. The
keyboard cursor (`▶`, inverse video) is not the attached pane (`▸`): one is where
a keypress goes, the other is what is streaming, and the frame shows both. `Esc`
dismisses whatever is open — the picker, the key list, an applied search — and
only quits when there is nothing left to dismiss. Ctrl-C on `arreo attach`
detaches and leaves the pane running — the pane belongs to the daemon, not to
your terminal.

The fleet keys are the CLI verbs with their refusals — the TUI never invents a
second sentence for a refusal the CLI would print:

- `s` spawns a pane. The prompt takes `<id> <program> [args…]`, quoting like a
  shell, so `s` then `build /bin/sh -c 'make -j4'` works; a refused spawn says
  why (`spawn: pane "build" already exists`).
- `i` sends to the **attached** pane — attach one with `Enter` first. The send
  is audited as this device, exactly like `arreo send`.
- `x` kills the attached pane **behind a confirmation that names it**; `y`
  confirms, Esc/N cancels.
- `m` opens the account's machines (`a` add from a pairing code, `r` rename,
  `x` remove — an online machine's remove asks for the same `f` force the CLI
  requires). `--machine <name>` flips the whole session's target, so a TUI
  pointed at another machine manages *that* machine's panes; its trust panel
  still refuses with the CLI's "trust is local" sentence, because a grant
  recorded anywhere but the enforcing machine is advice, not access.
- `g` opens this machine's trust grants (`a` grant a pinned device,
  fingerprint-confirmed like `--yes`'s prompt; `x` revoke, also confirmed).

Safety is a rendered fact, not a round-trip: a TUI whose device role cannot
spawn/kill/send shows the daemon's own `VerbDenial::Role` sentence as the reason
and draws the keys disabled, *before* any keypress — a viewer sees why, instead
of watching a refusal happen. `q`/Esc quits; the `?` key list shows every binding
with its confirmation requirement.

Two things are worth knowing before a long session. The pulse on a `question`
group is the terminal's own slow blink, so it costs no frames; if you want it
still, put `[tui] reduce_motion = true` in the config file the daemon already
reads (`--config PATH` or `$ARREO_CONFIG`), and `NO_COLOR` implies the same.
And at 80×24 the frame is laid out for it — the sidebar gives up columns before
the pane region does — so a small terminal degrades instead of glitching.

If you want the daemon managed for you instead of started by hand, `arreo service
install` writes the unit for this OS and, on Linux with a systemd user session,
enables and starts it.

## What each `cargo xtask` command does

`cargo xtask` is a cargo alias for `cargo run -p xtask --` (`.cargo/config.toml`),
so the two spellings are the same command.

| Command | What it actually does |
| --- | --- |
| `cargo xtask e2e --slice <name>` | One slice of the battery: `chaos`, `api`, `compat`, `lifecycle`, `persistence`, `state`, `enforcement`, `tui`, `theme`, `relay`. Each drives the real binaries; none is a mock. |
| `cargo xtask e2e` | Bare form: **a stub today.** It prints `not implemented` and exits 0, so it is not evidence. Name your slices. |
| `cargo xtask bench` | Spawns N real panes, replays recorded fixtures, and asserts the rows of `perf-budget.toml` that are live today. A regression fails the command. |
| `cargo xtask bench --probe proto` / `--probe size` | One measurement only: the 1 MB MessagePack codec check, or the release daemon's binary size. |
| `cargo xtask demo phase0` | Runs the Phase 0 exit demo — bench, a live 10-pane daemon, chaos, `conpty-smoke`, `check-targets` — and writes the result to `.loop/PHASE-DONE.md`. |
| `cargo xtask conpty-smoke` | PTY smoke test: the real ConPTY backend on Windows, the same `Pane` mechanics through `sh` on unix. |
| `cargo xtask check-targets` | Cross-compile gate: does our Rust `cargo check` for `x86_64-pc-windows-msvc`? `--enforce` turns a SKIP into a failure. |
| `cargo xtask adapters --check` | Lints every `adapters/*.toml` and replays that adapter's recorded fixtures, asserting the resulting state timeline. |
| `cargo xtask package --dry-run` | Validates the cargo-dist skeleton: the release targets and installers it declares, without building anything. |
| `cargo xtask release-check` | The public-readiness gate: fmt, clippy, test, vet, audit, deny, license, REUSE, secret scan and the docs link check, each with its own verdict. |

The e2e/bench verbs cover the Phase-0 surface; slices whose mechanism has not
landed yet print `not implemented` and exit 0, unless you pass `--enforce`, which
makes "stub" fail like a gate should. `cargo xtask release-check --public` is
stricter still: a scanner that is not installed is a failure rather than a skip.
That mode is for the launch gate, not for a contributor's first run.

## Where state lives

Nothing is hidden and nothing is a database you have to go hunting for. Paths are
derived from the socket and from XDG, and the rules are in the code, not in this
table — but this table is what the code does.

| What | Where | Derived by |
| --- | --- | --- |
| The socket | `$XDG_RUNTIME_DIR/arreo.sock`, else `/tmp/arreo-<uid>.sock` | `default_socket()` in `arreo_core::mesh::session`, `arreo-server`'s `main`, and the TUI client — one rule. Override per invocation with `--socket PATH`. |
| The store | `<socket>.db` — the sidecar SQLite (WAL) file next to the socket | `sidecar_db()`, `crates/arreo-core/src/identity/authority.rs`. Holds pane topology, scrollback, metrics history, the audit table and the device registry. |
| The audit trail | the `audit` table inside that same `<socket>.db` | `arreo audit` reads it directly, with no daemon round-trip, because the log outlives the daemon. See [audit.md](audit.md). |
| Long-lived identity | `$ARREO_IDENTITY_DIR`, else `$XDG_DATA_HOME/arreo`, else `$HOME/.local/share/arreo` | `identity_dir()` in `arreo_core::identity::keys`. |
| Keys and certificates | `<identity>/identity/root.key` (the server's root), `identity/devices/<id>.cert` (pinned devices) | `identity_root()`, same file. Mode 0600/0700. |
| The machine-directory cache | `<identity>/identity/machines.cache` | `arreo machines`; a mirror that a successful relay read replaces whole, never a source. |
| Themes | `$ARREO_THEME_DIR`, then `$XDG_CONFIG_HOME/arreo/themes`, then `<repo>/.arreo/themes`, then `./.arreo/themes` | `default_dirs()` in `arreo_core::theme::loader`. Five themes are embedded in the binary (`arreo`, `tokyonight`, `catppuccin`, `gruvbox`, `system`), so a static build still has a theme with no data dir. The `arreo` built-in *is* [`design/BRAND.md`](../design/BRAND.md) §2, token for token, and a test compares the two files rather than a copy of the table. |
| TUI settings | the `[tui]` section of the file named by `--config` or `$ARREO_CONFIG` | `TuiSettings::load` in `arreo_core::relay::config` — one parser for the one config file, so the TUI and the daemon cannot disagree about what it means. Today: `reduce_motion`. |
| Relay configuration | the file given by `--config PATH` or `$ARREO_CONFIG` | Shared by the daemon and the CLI (`arreo_core::relay::config`). A `[relay]` section with `enabled = true` starts the outbound session; **disabled is the default**, and no relay is needed for a local-only machine. |
| The daemon's service unit | `~/.config/systemd/user/arreo.service`, or `~/Library/LaunchAgents/dev.arreo.daemon.plist` | `unit_path()` in `arreo_core::lifecycle`, written by `arreo service install`. Windows has no unit file: the verb prints the `sc.exe` script instead. |
| Relay state | `<state-dir>/relay.db`, plus the pairing mailbox socket/tcp listener | `arreo-relay serve --state-dir DIR`. See [relay-deploy.md](relay-deploy.md). |

Two consequences worth internalizing. First, `--socket` is the *only* thing that
decides which daemon you are talking to, so a test fixture's daemon is one flag
away and cannot collide with your working daemon. Second, because the store is a
sidecar of the socket, moving a socket moves everything about that machine's panes
with it.

## Where to look for what

| Your question | The file that answers it |
| --- | --- |
| How do I build, test, and prove things in this repo? | [AGENTS.md](../AGENTS.md) |
| What is this product supposed to be, and what is not built yet? | [ROADMAP.md](../ROADMAP.md) — read §6 for phases and §7 for the license split |
| How do I contribute, and what are the hard rules? | [CONTRIBUTING.md](../CONTRIBUTING.md) |
| What exactly does the daemon record, redact, and refuse to record? | [docs/audit.md](audit.md) |
| How do machines find each other? | [docs/machines.md](machines.md) |
| How does a client talk to a daemon on another machine? | [docs/relay-protocol.md](relay-protocol.md) |
| How do I run my own relay? | [docs/relay-deploy.md](relay-deploy.md) |
| Which OS does what, and what is only proven by CI? | [docs/cross-os.md](cross-os.md) |
| What is actually known about Windows/ConPTY? | [docs/conpty-windows.md](conpty-windows.md) |
| How do I drive Arreo from inside a coding agent? | [docs/agent-skill.md](agent-skill.md) |
| How are releases built and signed? | [docs/release.md](release.md) |
| Why is a design decision the way it is? | [specs/adr/](../specs/adr/) — one ADR per decision |
| What is the task protocol the agent loops use? | [tasks/README.md](../tasks/README.md) and [AGENTS.md](../AGENTS.md) |

## Status, honestly

Arreo is **pre-1.0 and pre-launch**. There are no releases, no published crates,
no install script and no `arreo.dev` — the domain does not resolve, so do not use
`arreo.dev` URLs, `security@arreo.dev`, `conduct@arreo.dev`, the `arreo.dev`
install commands or the `arreo/homebrew-arreo` tap as if they worked. The repository
itself is already public (`ivan-cavero/Arreo`), which is why this document is
careful about what is and is not named. [docs/release.md](release.md) says the same
in place rather than pretending otherwise. Everything you can run today you build
from this checkout.

What that means in practice:

- The workspace version is `0.1.0` and the daemon/CLI/TUI/relay all build from
  source today.
- The local path — daemon, CLI, TUI, panes, states, metrics, audit, themes,
  persistence across a daemon restart — is the part that is daily-usable.
- The remote path — pairing, device certificates, the relay, the machine
  directory, cross-machine attach — is implemented and covered by tests, but it
  is not something a stranger can be walked through without first running a relay.
- Phones, the web client, config sync, auto-update, plugins, push notifications
  and the approvals gates are **not built**. They are described in the roadmap as
  future work; they are not described here as features.

The license split is real and enforced today: **Apache-2.0** for everything in
`crates/` except `crates/arreo-relay*`, which is **AGPL-3.0-or-later**. Nothing
Apache-licensed may depend on the relay — `cargo test -p xtask --test
workspace_deps` fails the build if that direction is ever reversed, and
`cargo xtask release-check` re-checks the declared licenses. See
[CONTRIBUTING.md](../CONTRIBUTING.md) for what the boundary means when you build
on the relay.
