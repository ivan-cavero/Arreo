# Contributing to Arreo

First: thank you. Arreo is open core on purpose — the runtime is Apache-2.0 and stays that
way, and the project is built to be extended by strangers: **adapters, themes, docs, and
code** all have first-class contribution paths. This document tells you how each one works.
(Plugins do not have one yet — the runtime that would load them is not built; see the
roadmap.)

> New to the codebase? [docs/tour.md](docs/tour.md) explains how the pieces fit,
> [ROADMAP.md](ROADMAP.md) is the design, and `specs/adr/` holds one architecture
> decision record per decision. The whole project is developed in agent loops with
> human review as the gate — see §10 — so "read the tasks file" is real advice here.

## Code of Conduct

Be excellent to each other. See [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) (Contributor
Covenant). Note that the `conduct@arreo.dev` address in that file is pre-launch: the
`arreo.dev` domain does not resolve yet, so it bounces. Until launch, raise conduct
matters in an issue marked for a maintainer's attention.

## Ways to contribute

| Path | Where | Skill floor |
| --- | --- | --- |
| **Agent adapter** (state detection for a harness) | `adapters/*.toml` — data, no Rust required | low |
| **Theme** | `crates/arreo-core/themes/*.json` — one JSON file | lowest |
| **Docs** | `docs/` — always underappreciated, always merged fast | low |
| **Core code** | Rust, `crates/*` | medium |
| **Security review** | coordinated disclosure, see [Security issues](#security-issues) | expert |

## Development setup

```console
# Linux / macOS
git clone https://github.com/ivan-cavero/Arreo && cd Arreo
cargo build --workspace    # arreo-server, arreo, arreo-tui, arreo-relay
cargo test --workspace     # unit suite, incl. the dependency-direction gate
cargo xtask e2e            # the end-to-end battery, once the binaries exist

# Windows notes
# - Windows 10 1809+ required (ConPTY); Windows Terminal is the supported baseline
# - Developer Mode recommended for symlinked test fixtures
# - UTF-8: the daemon forces the codepage; keep your shell on UTF-8 too
```

Requirements: stable Rust, pinned via `rust-toolchain.toml`. Nothing else is needed to
build and test locally; `cargo xtask e2e --slice lifecycle` exercises the service units the
way the OS's own manager would. `.github/workflows/ci.yml` is the multi-OS gate — it is
written to run fmt, clippy, the test suite, the supply-chain checks (`cargo vet`, `cargo
audit`, `cargo deny`, versions pinned in that workflow), the portability gate and the e2e
slices on Linux, macOS and Windows. **It is red on all three legs today**, so a green run
is not evidence you can cite; [docs/cross-os.md](docs/cross-os.md) says which portability
claims rest on which layer.

### Everyday commands

```console
cargo xtask e2e --slice state   # one real slice (see the list below)
cargo xtask e2e --slice chaos   # the adversarial battery
cargo xtask bench               # perf budgets from perf-budget.toml — regressions fail
cargo xtask conpty-smoke        # PTY smoke; the real ConPTY backend on Windows
cargo xtask check-targets       # cross-compile gate for the Windows target
cargo xtask adapters --check    # lint adapters/*.toml + replay their fixtures
cargo xtask release-check       # fmt, clippy, test, supply chain, licenses, links, secrets
cargo clippy --workspace --all-targets -- -D warnings   # zero warnings is the bar
cargo audit && cargo vet && cargo deny check            # supply chain
```

The real e2e slices are `chaos`, `api`, `compat`, `lifecycle`, `persistence`, `state`,
`enforcement`, `tui`, `theme` and `relay` — each a thin runner over integration tests that
drive the real binaries on a real PTY. **Bare `cargo xtask e2e` with no `--slice` is still a
stub that prints `not implemented` and exits 0**, so it is not evidence; name the slices you
ran. Several need a daemon, a relay or a PTY, and the `enforcement` slice reports honestly
whether this box can delegate a cgroup rather than pretending it passed.

## The rules (enforced, not aspirational)

1. **Strict TDD.** No production change without a failing test first. Workers in agent
   loops follow the same rule; "it compiles" is not a test.
2. **The E2E battery gates everything.** A PR must pass the slices it touches on Linux,
   macOS, *and* Windows — `.github/workflows/ci.yml` is written to run the whole battery
   on all three, and a release is not a release if any OS is red. Two caveats while the
   project is pre-launch: that workflow is currently failing on all three legs, so a
   "CI is green" claim is not available to cite; and bare `xtask e2e` with no `--slice`
   is a stub that proves nothing. Name the slices you ran.
3. **Perf budgets are executable.** If `xtask bench` says you regressed a budget row, fix
   it — no human judgment, no "but it's small". Every row in `perf-budget.toml` carries
   `phase0 = true` (asserted today) or `false` (the target, skipped by name with a note).
4. **Dependencies are reviewed, not added.** Std + in-tree first; a new crate needs a
   ledger note with rationale (correctness/performance/security/simplicity/maintenance
   — "it's popular" is not a rationale). No scaffolding dependencies (implementable in
   an afternoon). Gates: `cargo vet` (reviews) + `cargo audit` (RUSTSEC) + `cargo deny`
   (bans/duplicates/licenses) — zero unresolved findings or the merge fails. Vetting a
   new dep means `cargo vet certify` (exemptions are the day-one floor, not the goal).
5. **No secrets, ever.** Do not commit keys, even "test" ones. The secret-shape scanner
   guards the paths that could leak one — `arreo record` refuses to write a fixture that
   looks like a secret without `--allow-secrets`, and the daemon redacts before anything
   reaches the audit log or disk. `cargo xtask release-check` adds a full-history scan
   (`gitleaks`, pinned) on top of that.
6. **Compat window.** Protocol/schema changes must keep N−1 client compatibility, and
   `cargo xtask e2e --slice compat` proves it in both directions. There is no live-update
   handoff yet (nothing in this tree upgrades a running daemon), so for now every schema
   change is simply a normal breaking-change review.
7. **Small PRs.** One concern per PR; target < ~400 changed lines. Larger work goes in
   stacked PRs — each one reviewable on its own, merged in order.

## Pull requests

- Fork → branch → worktree (we use git worktrees heavily; so can you).
- Every PR states its **evidence**: which e2e slices passed, on which OSes, and the bench
  delta if you touched anything hot. "Tests green" links, not vibes.
- UI changes include a screenshot or terminal capture; theme changes include before/after.
- One approval required; two for `crates/arreo-relay*` (AGPL component, higher scrutiny).
- DCO sign-off on every commit: `git commit -s` → `Signed-off-by: Name <email>`
  (Apache-2.0/DCO-1.1 convention; we don't require a CLA).

## Contributing an adapter (the highest-value, lowest-barrier contribution)

An adapter is data: TOML, no Rust required. It tells the state engine what a harness's
prompt and error shapes look like, and how long silence has to last before it means
anything. The schema is flat and strict — an unknown key is a parse error, because a
typo'd pattern name that silently disabled detection would be worse than a red build:

```toml
# adapters/myharness.toml — every key optional except the ones you need;
# defaults are 2000/2000/2500 ms, bell and done_on_exit true.
idle_after_ms       = 2000     # silence + a plain prompt tail → idle
question_after_ms   = 2000     # silence + a prompt-shaped tail → question
blocked_after_ms    = 2500     # silence after an error shape → blocked
bell_means_attention = true    # BEL with no output → question/blocked
done_on_exit        = true     # child exit always emits done (with its code)

# Regexes matched against the last visible lines after silence. First match
# wins and its text rides on the event as the matched pattern — which is what
# makes an `inferred` state auditable rather than a guess.
question_patterns = ["\\[y/n\\]", "\\(y/n\\)", "proceed\\?", "❯\\s*$"]

# Output shapes that arm the `blocked` transition once silence follows.
error_patterns = ["Traceback \\(most recent call last\\)", "(?m)^(Error|FAILED|FATAL):"]
```

`adapters/default.toml` is the universal tier and the one the daemon runs today;
`adapters/opencode.toml` and `adapters/pi.toml` are recorded per-harness shapes. Two
things to know before you write a new one:

- **Check it against real sessions, not against your prose.** Record the harness once
  with `arreo record <command> -o fixtures/myharness-question.pty`, commit the fixture,
  then run `cargo xtask adapters --check`. That verb lints every `adapters/*.toml` and
  replays each adapter's fixtures through the real engine, asserting the end state.
- **A new adapter replays no fixtures until it is registered.** The fixture list is a
  `match` in `xtask/src/adapters_check.rs` (`adapter_fixtures`), one arm per harness,
  naming files in the flat `fixtures/` directory. Add your arm in the same PR — an
  adapter that nothing replays is a claim, not a test.

Per-harness adapter *selection* is not wired yet: the daemon runs the embedded default
adapter for every pane, so a new adapter's value today is a better universal pattern set
plus the fixtures that prove it. If the harness has an official hook or event stream, say
so in the PR — the native-tier hooks are specified in `adapters/*-native.md` and are the
next step after selection lands.

## Contributing themes

A theme is one JSON file. Copy any built-in from `crates/arreo-core/themes/` (`arreo.json`
is the reference), fill the semantic tokens with truecolor hex, and open a PR —
`"none"` as a value means "inherit whatever the terminal already has". Five themes are
embedded in the binary (`arreo`, `tokyonight`, `catppuccin`, `gruvbox`, `system`), and
the loader also reads `$XDG_CONFIG_HOME/arreo/themes` and `./.arreo/themes`, so you can
iterate on a file locally before proposing it. There is no theme gallery to submit to
yet; the PR is the gallery.

## Plugins

There is nothing to contribute to yet: `arreo-plugin-api` is an empty shell that fixes
the crate name and the license boundary, and no code loads a plugin. The design — WASM
components with capability-gated manifests — is roadmap work, and a PR against it would
be speculative until that runtime exists.

## Working on the relay (AGPL)

`crates/arreo-relay*` is AGPL-3.0-or-later. Contributions to it are licensed the same way;
the project retains the right to offer managed-relay exceptions (dual-tracking applies to
the relay only, never to the Apache core). If in doubt, ask before building on it.

### The license boundary (enforced by `cargo test -p xtask --test workspace_deps`)

Apache code may enter an AGPL work; AGPL code may not enter an Apache work (the binary
would be AGPL). So the dependency rule points one way, and a machine checks it:

- `arreo-relay` may depend on Apache first-party crates — one-way, today only
  `arreo-core` (the protocol vocabulary, which is what makes the relay implementable
  from other code).
- **No Apache first-party crate may depend on `arreo-relay`** — not `arreo-core`,
  not `arreo-server`, not `arreo-cli`, not `arreo-tui`, not `arreo-plugin-api`.
  Other-licensed code interoperates over the documented protocol
  (`docs/relay-protocol.md`) or by running the unmodified `arreo-relay` binary.
- Every shipped crate declares Apache-2.0; the relay declares AGPL-3.0-or-later
  explicitly (inheriting the workspace default would silently license it Apache).
- `arreo relay serve` stays unimplemented on purpose: linking the relay into
  `arreo-cli` would relicense the CLI, and an exec shim is a second name for one
  thing. Run the relay binary; do not wrap it.

## Agent-loop contributions (yes, an agent can open your PR)

Arreo is [developed in agent loops](ROADMAP.md) — machine-driven workers, human-approved
merges. If you contribute that way:

1. Pick a task from `tasks/` — acceptance criteria are written down; if they're not,
   propose them first. Agents don't take tasks from chat context.
2. One worktree per task; strict TDD; the worker's exit condition is tests + e2e slice +
   self-review.
3. An independent read-only verifier runs the battery against your diff — never trust the
   writer's own green.
4. A human (or native review) approves the merge. Agents don't merge.

## Security issues

**Do not open public issues for security reports.** The reporting channel is
[SECURITY.md](SECURITY.md): read it first, because it states plainly what works today and
what does not. In short — the mechanism is GitHub's private vulnerability reporting, whose
report URL for this repository is
`https://github.com/ivan-cavero/Arreo/security/advisories/new`, but the setting is
**disabled on this repository as of 2026-09-12**, so there is no working private channel
yet. Until it is enabled, do not paste a repro anywhere public: write only that you have a
security report and need a private channel, and a maintainer will arrange one.
`security@arreo.dev` (the address this section used to print) and `arreo.dev/security` do
not exist pre-launch — the domain has no DNS record, so mail to it bounces.

The disclosure window is 90 days from the report, matching SECURITY.md, and we publish
advisories and credit reporters. In scope is anything that breaks a promise the code makes
— pairing, device identity and per-machine trust, relay auth and routing, the remote
transport, the daemon's socket boundary, secret redaction, and the Linux resource guard —
with the full table, and the exclusions, in [SECURITY.md](SECURITY.md).

## Code of conduct enforcement & contact

- Conduct: see [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) — its `conduct@arreo.dev` address
  is pre-launch (the domain does not resolve), so raise it in an issue until launch.
- General: [the issue tracker](https://github.com/ivan-cavero/Arreo/issues) — GitHub
  Discussions is **not enabled** on this repository, and there is no Discord server (nor
  an `arreo.dev` to host one).

*The Arreo name is a promise: nobody herds agents alone.*
