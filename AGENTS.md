# AGENTS.md — operating Arreo's repo with agents

> Source of truth for commands. Specs live in `ROADMAP.md` + `specs/`;
> work lives in `tasks/`. This file is only how to build, test, and prove things.

## Layout

Workspace crates: `arreo-core` (PTY, VT state, state engine, metrics, protocol,
device identity, pairing, themes), `arreo-server` (daemon), `arreo-cli` (`arreo`
binary), `arreo-tui` (ratatui client: sidebar, pane wall, theme picker),
`arreo-relay` (AGPL: pairing mailbox today, routing/presence next),
`arreo-plugin-api` (WASM plugin host, Phase 4+). Dev tooling: `xtask`.

**Dependency direction (enforced by `cargo test`, see `xtask/tests/workspace_deps.rs`):**
`arreo-server` and `arreo-cli` depend on `arreo-core`; nothing except `xtask`
(dev tooling, never shipped) may depend on `arreo-server`.

## Toolchain

Pinned in `rust-toolchain.toml` (current stable). Do not upgrade without a task.

## Commands

```console
cargo build --workspace                  # build everything
cargo test --workspace                   # full unit suite (incl. dep-direction gate)
cargo clippy --workspace --all-targets -- -D warnings   # must be zero warnings
cargo fmt --all -- --check               # must be clean
cargo xtask e2e                          # e2e battery (stubs until wired per task)
cargo xtask e2e --slice tui              # TUI: sidebar/wall/mouse on a real pty
cargo xtask e2e --slice theme            # theming: depth fallback + shared tokens
cargo test -p arreo-cli --test pairing   # pairing: three real processes (needs arreo-relay built)
cargo xtask bench                        # benchmarks vs perf-budget.toml
cargo xtask conpty-smoke                 # Windows ConPTY smoke (T-0007)
cargo xtask <cmd> --enforce              # fail when budgets exist but unmet
cargo run -p arreo-cli -- --version      # CLI smoke
```

`cargo xtask` is a plain cargo alias for `cargo run -p xtask --` (`.cargo/config.toml`),
so the documented form and the long form are the same thing.

## Task protocol

Pick from `tasks/`: highest priority with `status: todo` whose `depends_on` are all
`done`. Scope fence in frontmatter is law — outside the fence is a bug, even if it
looks like an improvement. Acceptance criteria before code (TDD on personal edits;
workers write failing-test-first). Update `status:` as you go (`todo` →
`in-progress` → `done` with evidence).

## Evidence (PROMPT.md §6)

Claims without artifacts don't merge. Per task: tests + e2e slice + bench numbers
under `.loop/evidence/<TASK-ID>/`, referenced from the ledger (`.loop/PROGRESS.md`).
After it works, run an adversarial pass (malformed input, zero-length, huge output,
rapid toggling, concurrent use); findings become tasks or in-scope fixes.

## Cross-OS (dev runs on Linux)

Cross-compile gates (`cargo xtask check-targets`, T-0010) prove *it builds*;
GitHub-hosted runners prove *it behaves*. Wine is a quick check only, never a
shipping claim. Never fake macOS results; never run macOS on non-Apple hardware.

## Dependencies

Std + what's already in `Cargo.toml` first. New dependency = ledger note with
rationale. No scaffolding dependencies (implementable in an afternoon).
`cargo vet` + `cargo audit` + `cargo deny` gates land in T-0020.
