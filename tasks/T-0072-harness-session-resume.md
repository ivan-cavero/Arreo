---
id: T-0072
title: Harness-aware session resume — reattach resumes pi/opencode sessions, not just scrollback
phase: 4
priority: 2
status: proposed
depends_on: [T-0017, T-0018]
scope:
  - adapters/**
  - crates/arreo-core/src/state/**
  - crates/arreo-core/src/store.rs
  - crates/arreo-server/src/persist.rs
  - crates/arreo-server/src/daemon.rs
  - crates/arreo-cli/src/main.rs
  - crates/arreo-server/tests/persist.rs
  - xtask/src/persistence_slice.rs
  - xtask/src/main.rs
  - .loop/evidence/T-0072/**
verify:
  - cargo test --workspace
  - cargo clippy --workspace --all-targets -- -D warnings
  - cargo fmt --all -- --check
  - cargo xtask e2e --slice persistence
---

## Goal

Today's restore (T-0018, `persist.rs`) respawns the recorded command and pre-seeds
scrollback as visual history — the child is fresh, the harness session is not resumed.
Closing and reopening must resume the harness session where the harness supports it
(`opencode --session <uuid>`, pi resume), falling back to today's respawn+history
where it does not.

## Acceptance criteria

- [ ] Per-pane record carries `harness + session-id` alongside program/args (schema
      migration, old rows restore as today — no orphan DBs).
- [ ] Each adapter declares its resume strategy in data (`adapters/*.toml` + native
      maps): resume argv builder for pi + opencode, explicit `none` for harnesses
      without resume. No per-harness code branches outside the registry.
- [ ] Boot restore uses the resume argv when present; resume failure falls back loudly
      to respawn+history (one bad record never blocks the rest, same contract as T-0018).
- [ ] Proven live against real pi + opencode CLIs: spawn → produce session → kill -9
      daemon → restart → pane continues the harness session (session continuity asserted
      via the harness, not just byte-equal scrollback). Unknown-harness pane proves the
      fallback path unchanged.
- [ ] Safety: resume argv is constructed, never replayed keystrokes (T-0018's proven
      hazard stays fixed); session ids are not secret-scanned away and never logged
      in plaintext beyond what the harness itself prints.

## Design (planner, 2026-09-13 — probed live on this box before specifying)

The criteria above leave the *representation* of "resume strategy" open. Probed the two real
CLIs rather than assuming:

- **pi 0.84.4** can have its session **pinned by the caller**: `--session-id <id>` ("use exact
  project session ID, creating it if missing"), and `--session <path|id>` resumes an existing
  one. In `--mode json` the first line is `{"type":"session",...,"id":"<uuid>",...}` (verified:
  a real run emitted `01a099cd-2d35-74c4-90c6-d0e3b10011b5`). So for pi Arreo *generates* the
  id at spawn, passes it, and resumes with it — deterministic, no output capture needed.
- **opencode 1.18.30** generates its own ids (`ses_<...>`) and its **interactive TUI prints no
  session id** (verified: 25 s of a real pty capture, zero `ses_` occurrences) — only
  `--format json` events carry `"sessionID":"ses_..."`. But it has `-c/--continue` ("continue
  the last session"), which needs no id at all. So opencode's strategy is *continue-by-default*
  for an interactive pane, with the id captured when the pane happens to print one (the JSON
  mode), and `--session <id>` used when the record has one.

So the data model is a **strategy**, not just an argv template:

```toml
# adapters/pi.toml
harness = "pi"
[resume]
kind = "pin"                                   # Arreo chooses the id at spawn
argv = ["--session-id", "{session}"]
session_pattern = '"type":"session".*?"id":"([0-9a-f-]{36})"'   # belt: also capture

# adapters/opencode.toml
harness = "opencode"
[resume]
kind = "continue"                              # the harness picks; resume by continuation
argv = ["--continue"]
exact_argv = ["--session", "{session}"]        # when the record has an id
session_pattern = '"sessionID":"(ses_[A-Za-z0-9]+)"'

# adapters/default.toml  (and any adapter without a [resume] table)
harness = "none"                               # respawn + history, unchanged (T-0018)
```

`kind` is the branch the *registry* takes, so no per-harness code exists outside the data: `pin`
→ add `argv` at spawn with a generated id, record it, use `argv` again to resume; `continue` →
add `argv` to resume only; `none` → today's path. The record carries `harness` + `session_id`
(the criterion), and the *proof* of continuity is the harness's own store (pi's session file /
opencode's session listing), not the pane's scrollback.

## Verification

```console
cargo test --workspace
cargo xtask e2e --slice persistence
```
