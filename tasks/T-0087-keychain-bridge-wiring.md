---
id: T-0087
title: Wire the keychain bridge into the daemon's spawn — a synced reference resolves at the PTY
phase: 4
priority: 2
status: done
depends_on: [T-0083]
scope:
  - crates/arreo-core/src/pty.rs
  - crates/arreo-server/src/daemon.rs
  - crates/arreo-server/src/persist.rs
  - crates/arreo-core/src/sync/keychain.rs
  - crates/arreo-server/tests/**
  - .loop/evidence/T-0087/**
verify:
  - cargo test --workspace
  - cargo xtask e2e --slice persistence
---

## Goal

T-0083 shipped the keychain bridge's **mechanism** — `keychain::plan(...).environment()`
returns the exact env a PTY spawn must apply, proven at a real pty in the slice — but the
**production wiring** is not built: the daemon's `Pane::spawn` takes no environment, so a
synced `{env:VBK_PROD_KEY}` config only resolves if the variable happens to be in the
daemon's own environment. §3.8's promise is that the machine's keychain supplies the value,
which means a daemon started by systemd (no shell, no exports) must still give the harness
its key.

## Acceptance criteria

- [x] **The minimal, principled injection set.** The daemon resolves, for a pane about to
      spawn, the synced files of the adapter that pane runs under (T-0072's registry), takes
      the env-reference names those files carry, and injects the keychain values for exactly
      those names — never the whole keychain. A pane whose adapter has no synced files gets
      no injection, and the daemon's own environment still wins for a name it already has
      (the keychain is the fallback, not the override).
- [x] **`Pane::spawn` gains an env parameter** (or a `spawn_with_env` sibling — whichever
      keeps the two existing call sites honest) without changing the default behaviour: the
      current callers pass nothing and get today's env.
- [x] **Proven at the product surface**: in an isolated daemon (own identity, own
      `$XDG_CONFIG_HOME`), a synced pi file referencing `VBK_PROD_KEY` is set only in the
      keychain store; a spawned pi pane's child process has the variable (assert via a pane
      that prints `env | grep`), and a pane whose adapter has no synced files does not.
      A daemon with the variable in its own environment keeps its own value even when the
      keychain differs.
- [x] The by-name report (`arreo sync env`) and the receive-time refusal from T-0083 stay
      the operator-facing doors; the injection is invisible to them.
- [x] No new dependency; `--slice persistence` and the handoff suites stay green (the
      spawn signature change touches the restore path).

## Landed (2026-09-14)

`keychain::spawn_environment(harness, env)` reads only the harness's `Class::Sync` files,
collects the names they carry, and resolves them from this machine's store; the daemon
applies the result at both spawn sites (Spawn, Split) and `persist::restore` applies it on
both of its paths (the resume branch and the plain fallback) — a restored pane is the same
pane. `Pane::spawn_with_env` is one implementation with two doors, so `spawn` and the
injecting spawn cannot drift.

Five tests in `crates/arreo-server/tests/keychain_spawn.rs`, 75 consecutive clean runs, and
four mutations each reddening the test it should — including "inject every name the store
holds", which is the leak this feature must not become (`.loop/evidence/T-0087/`). The
missing-secret notice names the variable and is asserted to carry no value.

## Notes

- The slice evidence from T-0083 (`.loop/evidence/T-0083/sync-check.txt`, the "injected"
      stage) is the mechanism's proof; this task adds the daemon wiring and re-proves at the
      pane level.
- Rejected: injecting the whole keychain into every pane (a leak-surface expansion — any
      program the operator runs would see every key), and resolving at the *CLI* only (the
      daemon owns the PTY; the CLI's own env is irrelevant to a pane).
- The daemon's environment-wins rule is what keeps a manual `export` working — T-0083's
      keychain already documents process-env as a fallback.