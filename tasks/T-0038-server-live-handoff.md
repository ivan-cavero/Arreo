---
id: T-0038
title: Server live handoff on Unix — PTY masters over SCM_RIGHTS, zero-cut update
phase: 2
priority: 2
status: in-progress
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

- [x] **Stage 0 — adopt primitive.** `Pane::adopt(master_fd, child_pid, size, SpawnSpec)` works, and
      so do the `send_fd`/`recv_fd` helpers it needs. 18 tests in `crates/arreo-core/tests/adopt.rs`
      drive the real chain (open a pty, spawn a child, dup the master, send it over a `socketpair`,
      receive, adopt) and assert through the adopted `Pane`: the ring carries what the child wrote
      *before* the handoff, input written through the adopted writer reaches that same child, a
      `resize` reaches the kernel and the child's own `stty size`, and the child's exit is observed
      without inventing a code. Required because `Pane` holds an opaque `Box<dyn MasterPty>`, which
      cannot be built from an fd today — `MasterPty` is a public trait, so the adopted master
      implements it rather than the crate being forked (ADR 0021).

      Three decisions inside this stage that the later stages depend on:
      - **Exit is never invented.** A process we did not fork cannot be waited for, so exit comes
        from `pidfd_open` (Linux ≥ 5.3) OR'd with end-of-stream on the master, decided once and
        cached; the status is `UNKNOWN_EXIT` (`u32::MAX`, outside the 0..=255 a real status
        occupies) because `0` would claim a clean exit nobody observed. On a kernel without pidfd
        this degrades to end-of-stream — a cost in sharpness, never a refusal to adopt, because
        refusing would give up a live agent to keep a status code.
      - **The adopted writer does not send EOT on drop**, unlike portable-pty's own. The outgoing
        daemon drops its handles at commit, so an EOT there would kill the very agent the handoff
        exists to save.
      - **The kernel's geometry wins.** A claimed `size` is applied only when the terminal has none,
        so a stale sender cannot resize a live agent's terminal through adoption.

      Found en route and fixed as its own task: **T-0071** (two daemons could serve one socket).

      **Security review of stage 0** (mandatory — fd passing is the critical surface) found seven
      issues; all are fixed, each with a test that fails without it. The one that mattered:
      **the claimed child pid was never cross-checked against the descriptor.** It arrives over the
      same untrusted channel as the master, and a pid is not inert — `pidfd_open` on a same-user
      process needs no permission check, so a wrong pid made `kill` signal an unrelated process and
      `try_wait` attribute its death to the pane. The pid is now accepted only when `TIOCGSID` on
      the adopted master names it, refused with both numbers when they disagree, and dropped
      (never watched, never signalled) when the terminal names no session at all — which is the
      ordinary shape of a pane that died mid-cut, not an error. Mutation-checked: removing the
      comparison turns `adopt_refuses_a_pid_the_terminal_disagrees_with` red.

      The other six were overclaims and one-flag or one-predicate defects: the byte that
      accompanies a descriptor is a *synchronisation marker*, not an integrity control (ancillary
      data is a barrier on a stream socket, so a channel carrying protocol bytes too makes
      transfers ambiguous — the fd channel must be dedicated, ADR 0021 §2b); an empty message on a
      packet socket is not end-of-stream; `send_fd` now passes `MSG_NOSIGNAL` so a peer dying
      mid-transfer cannot kill the sending daemon; a *partially* set geometry no longer counts as
      unset (it let a stale sender resize a live agent's terminal); the EOT and CLOEXEC comments
      now claim only what the code guarantees. `Pane::adopt`'s size argument became a named
      `AdoptSize { cols, rows }`, because a positional `(u16, u16)` would transpose silently
      against the crate's `PtySize` (rows-first) in the one branch where the value is used.

      **Cross-OS gate for a later stage:** `TIOCGSID` returning a session on a session-less master
      is measured on Linux, and [INFERENCE] from XNU on Darwin. If it differs there, macOS panes
      would adopt as already-exited — fail-closed but wrong. A macOS CI runner must settle it;
      no Linux build can.
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
