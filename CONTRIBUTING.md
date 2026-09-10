# Contributing to Arreo

First: thank you. Arreo is open core on purpose — the runtime is Apache-2.0 and stays that
way, and the project is built to be extended by strangers: **adapters, themes, plugins, docs,
and code** all have first-class contribution paths. This document tells you how each one works.

> New to the codebase? Start with [ROADMAP.md](ROADMAP.md) for the architecture and
> `specs/` for per-crate design docs. The whole project is developed in agent loops
> with human review as the gate — see §10 — so "read the tasks file" is real advice here.

## Code of Conduct

Be excellent to each other. See [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) (Contributor
Covenant). Violations: `conduct@arreo.dev`.

## Ways to contribute

| Path | Where | Skill floor |
| --- | --- | --- |
| **Agent adapter** (state detection for a harness) | `adapters/*.toml` — data, no Rust required | low |
| **Theme** | `themes/*.json` — one JSON file | lowest |
| **Plugin** | any WASM Component Model target (Rust scaffold provided) | low–medium |
| **Docs** | `docs/` — always underappreciated, always merged fast | low |
| **Core code** | Rust, `crates/*` | medium |
| **Security review** | coordinated disclosure, see §8 | expert |

## Development setup

```console
# Linux / macOS
git clone https://github.com/arreo-dev/arreo && cd arreo
cargo xtask setup          # installs hooks, checks toolchain
cargo build
cargo test

# Windows notes
# - Windows 10 1809+ required (ConPTY); Windows Terminal is the supported baseline
# - Developer Mode recommended for symlinked test fixtures
# - UTF-8: the daemon forces the codepage; keep your shell on UTF-8 too
```

Requirements: stable Rust (pinned via `rust-toolchain.toml`), plus the platform service
tooling for integration tests (systemd user session on Linux CI, launchd on macOS,
`windows-service` on Windows runners — CI provides all three).

### Everyday commands

```console
cargo xtask e2e            # full E2E battery (the definition of "works")
cargo xtask e2e --slice state   # one slice, for fast iteration
cargo xtask bench          # perf budgets from perf-budget.toml — regressions fail
cargo xtask conpty-smoke   # Windows PTY sanity check
cargo clippy --workspace --all-targets   # zero warnings is the bar
cargo audit && cargo vet             # supply chain
```

## The rules (enforced, not aspirational)

1. **Strict TDD.** No production change without a failing test first. Workers in agent
   loops follow the same rule; "it compiles" is not a test.
2. **The E2E battery gates everything.** Your PR must pass `xtask e2e` on Linux, macOS,
   *and* Windows. A release is not a release if any OS is red — this applies to PRs too.
3. **Perf budgets are executable.** If `xtask bench` says you regressed RSS or attach
   latency, fix it — no human judgment, no "but it's small".
4. **Dependencies are reviewed, not added.** Std + in-tree first; a new crate needs a
   ledger note with rationale (correctness/performance/security/simplicity/maintenance
   — "it's popular" is not a rationale). No scaffolding dependencies (implementable in
   an afternoon). Gates: `cargo vet` (reviews) + `cargo audit` (RUSTSEC) + `cargo deny`
   (bans/duplicates/licenses) — zero unresolved findings or the merge fails. Vetting a
   new dep means `cargo vet certify` (exemptions are the day-one floor, not the goal).
5. **No secrets, ever.** Config sync code includes secret-shape scanning; `cargo vet` and
   CI secret scans back it up. Don't commit keys, even "test" ones.
6. **Compat window.** Protocol/schema changes must keep N−1 client compatibility or be
   behind a feature flag. If the live handoff can't carry your change, mark it deferred-update.
7. **Small PRs.** One concern per PR; target < ~400 changed lines. Larger work goes in
   stacked PRs (the chained-prs skill documents the pattern).

## Pull requests

- Fork → branch → worktree (we use git worktrees heavily; so can you).
- Every PR states its **evidence**: which e2e slices passed, on which OSes, and the bench
  delta if you touched anything hot. "Tests green" links, not vibes.
- UI changes include a screenshot or terminal capture; theme changes include before/after.
- One approval required; two for `crates/arreo-relay*` (AGPL component, higher scrutiny).
- DCO sign-off on every commit: `git commit -s` → `Signed-off-by: Name <email>`
  (Apache-2.0/DCO-1.1 convention; we don't require a CLA).

## Contributing an adapter (the highest-value, lowest-barrier contribution)

Arreo's state engine uses a three-tier ladder: native events → ACP → universal heuristics.
Most harnesses just need a TOML adapter:

```toml
# adapters/myharness.toml
name = "myharness"
match.process = ["myharness"]
[tiers.universal]
question_patterns = ["\\?$", "\\(y/n\\)", "❯", "›"]
idle_after_ms = 4000
[tiers.native.hooks]        # optional: register arreo as an event sink
event_file = "~/.myharness/events.json"
```

Test it against real sessions with `cargo xtask adapters --check myharness`, open a PR with
a fixture replay (`fixtures/myharness/*.pty`) so CI can replay its output deterministically.
If the harness has an official plugin/hook system, a native tier is even better — we'll help.

## Contributing themes and plugins

- **Themes:** copy `themes/template.json`, fill semantic tokens (truecolor hex; `"none"`
  inherits the terminal), open a PR — or submit straight to the
  [theme gallery](https://arreo.dev/themes). One file, done.
- **Plugins:** `cargo xtask plugin new <name>` scaffolds a capability-gated WASM component.
  Declare only the capabilities you use (`read-agent-state`, `add-widget`, …) — the registry
  displays your manifest before install. Hot-load locally with a copy into
  `~/.config/arreo/plugins/`. Security review applies to registry submissions.

## Working on the relay (AGPL)

`crates/arreo-relay*` is AGPL-3.0-or-later. Contributions to it are licensed the same way;
the project retains the right to offer managed-relay exceptions (dual-tracking applies to
the relay only, never to the Apache core). If in doubt, ask before building on it.

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

**Do not open public issues for security reports.** Email
[security@arreo.dev](mailto:security@arreo.dev) (PGP key on
[arreo.dev/security](https://arreo.dev/security)), include a repro and impact assessment, and
expect coordinated disclosure with a 90-day window. We publish advisories and credit reporters.

## Code of conduct enforcement & contact

- Conduct: `conduct@arreo.dev`
- General: [Discord](https://arreo.dev/discord) · [GitHub Discussions](https://github.com/arreo-dev/arreo/discussions)

*The Arreo name is a promise: nobody herds agents alone.*
