---
id: T-0038
title: Server live handoff on Unix — PTY masters over SCM_RIGHTS, zero-cut update
phase: 2
priority: 2
status: proposed
depends_on: [T-0002, T-0012, T-0013, T-0018]
scope:
  - crates/arreo-server/src/handoff/**
  - crates/arreo-server/src/daemon.rs
  - crates/arreo-server/src/main.rs
  - crates/arreo-server/Cargo.toml
  - crates/arreo-core/src/pty.rs
  - crates/arreo-core/src/proto/**
  - crates/arreo-server/tests/handoff.rs
  - xtask/src/handoff_slice.rs
  - xtask/src/main.rs
  - perf-budget.toml
  - .loop/evidence/T-0038/**
---

## Re-scope (2026-09-12): the signing key is not on the critical path

`T-0036` was in this task's dependencies because the new daemon is *presumably* a verified
release. Reading the five stages, none of them needs that: stage 0 is adopting a PTY master
over `SCM_RIGHTS`, stages 1–2 are the handoff and the in-flight-output proof, stage 3 is
client reconnect plus the latency row, stage 4 is abort-safety, and the last criterion is
SQLite. **Every one of them is mechanism.** Verification is T-0037's half, and it is blocked
on key custody a human holds.

So the dependency is dropped here, on the same reasoning that split T-0037 (see
`tasks/T-0070-update-swap.md`), and the artifact source is explicit: the new binary is staged
at a path the operator names — `arreo update --from <path> --server`, or the equivalent —
exactly as the client swap does. When T-0036 lands, the verified path feeds the same seam.

What does **not** change: a handoff still refuses a binary whose protocol version is outside
the N−1 window (that check is in the criteria below and needs no signature). Verification
before *installing* stays T-0037's job; this task's job is that the install cannot lose an
agent.

## Goal

The hardest thing in Phase 2, and why "updates never kill agents" is a claim and not a hope (§3.13
zero-cut update, §5 handoff row, §8 risk map): a new daemon proves its version, adopts the old one's
PTY masters over SCM_RIGHTS, re-opens SQLite and serves — a failed step kills only the new one.

## Acceptance criteria (staged — each stage is evidence, not a plan step)

- [ ] **Stage 0 — adopt primitive.** `Pane::adopt(master_fd, child_pid, size, SpawnSpec)` works:
      a unit test passes a master fd over a socketpair and reads back through the adopted `Pane`
      what it wrote (ring, VT, resize). Required because `Pane` holds an opaque
      `Box<dyn MasterPty>`, which cannot be built from an fd today.
- [ ] **Stage 1 — handoff with 0 panes.** `arreo update --server` on an idle daemon: the new
      process connects, version handshake, takes over, old exits 0. Socket path and session id
      unchanged, no client sees a disconnect, `arreo status --json` shows the new pid, audit row
      `handoff v<from> → v<to> panes=0`.
- [ ] **Stage 2 — N panes with output in flight.** 8 panes emitting a monotonic marker stream;
      handoff under load → every pane pid unchanged, no marker lost, duplicated or reordered
      across the cut, and lines written before the cut still readable after it.
- [ ] **Stage 3 — clients reconnect transparently.** TUI + CLI attached through a stage-2 handoff
      reattach with an unchanged session id in < 2 s — flip `perf-budget.toml`'s recorded
      `server_handoff_reattach_s` row to enforced and assert it in `xtask bench`. Resume tokens
      stay valid, and input sent during the cut is acked once or refused as retryable.
- [ ] **Stage 4 — abort leaves the old daemon whole.** Kill -9 the new daemon at three points
      (before fd transfer, mid-transfer, after ack before commit): every pane alive, old daemon
      serving, clients unaware, a retry succeeding — `flock` keeps exactly one serving daemon.
- [ ] SQLite: old daemon checkpoints WAL and closes, new one re-opens the same file; an audit
      write during the cut loses nothing and raises no `database is locked`; a corrupt `-wal` heals
      per T-0018's rule. Windows routes to T-0039 rather than half-implementing this.

## Notes

- New dependency: `rustix` 0.38.44 (already in `Cargo.lock` via `portable-pty`), features
  `net`/`process`/`fs`, for `sendmsg`/`recvmsg` + `SCM_RIGHTS`, `fcntl`, `waitpid`/`pidfd_open`
  — nothing new enters the tree, and no hand-rolled `libc::sendmsg` unsafe surface.
- Honest gap #1: the new daemon cannot `waitpid` the old one's children (they are reparented to
  init when it exits), so exit is detected via the master fd's EOF/EIO — the read loop's existing
  path — with `pidfd_open` giving a definitive status on Linux ≥ 5.3; a pane dying mid-cut
  reports `exited (code unknown)` instead of a guess.
- Honest gap #2: relay session tokens (§3.13 step 3) travel through a `HandoffPayload` trait;
  until the relay crates land that field stays empty and clients re-handshake — which stage 3
  proves must be transparent anyway.
- Scrollback is disk-backed (T-0018), so the transfer is a pointer swap plus a manifest, not a
  bulk copy. The control plane is the versioned MessagePack protocol (N−1 window): a major-break
  handshake is refused, turning the update into a deferred one instead of an attempt.
- The `handoff` slice is wired here; T-0042 puts it on CI and chains it into the release story.

## Verification

```console
cargo test -p arreo-server --test handoff
cargo xtask e2e --slice handoff
cargo xtask e2e --slice handoff --case abort
```
