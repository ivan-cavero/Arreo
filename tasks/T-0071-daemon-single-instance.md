---
id: T-0071
title: Exactly one daemon serves a socket
status: done
priority: 1
depends_on: []
phase: 2
---

# Goal

Two daemons must never serve one socket. Today they can.

## Why this exists

Found while designing T-0038 (the server live handoff), which rests on the invariant
"exactly one daemon serves this machine". Reading `Daemon::serve` showed the invariant
was assumed rather than enforced:

```rust
if Self::is_live(&self.socket).await { return Err(...) }   // probe
let _ = std::fs::remove_file(&self.socket);                 // unlink
let listener = UnixListener::bind(&self.socket)?;           // bind
```

Probe → unlink → bind is not atomic. Two daemons starting together both probe a
not-yet-bound path (both see "dead"), both unlink, and the **second one unlinks the
first's listener** and binds its own. The machine is left with two live daemons, one of
them unreachable, both writing the same SQLite sidecar.

## Repro (ran before the fix)

```console
for round in $(seq 1 12); do
  D=$(mktemp -d); S=$D/a.sock
  for i in $(seq 1 8); do arreo-server --socket $S > $D/$i.log 2>&1 & done
  sleep 2; count live daemons; kill them; done
```

Result: `rounds=12 rounds-with-more-than-one-daemon=1` — round 6 had **two daemons
alive** on one socket. Rare (the window is microseconds), silent, and it survives because
nothing owns the path.

## Scope fence

`crates/arreo-core/src/lock.rs` (new), `crates/arreo-core/src/lib.rs`,
`crates/arreo-core/src/identity/authority.rs`, `crates/arreo-core/src/update/mod.rs`,
`crates/arreo-server/src/daemon.rs`, `crates/arreo-server/src/persist.rs`,
`crates/arreo-server/tests/single_instance.rs`. Nothing else.

## Acceptance criteria

- [x] A daemon holds an exclusive lock on `<socket>.lock` for its whole life, taken
      **before** the probe, so probe → unlink → bind is a critical section only the
      holder may enter.
- [x] A second daemon on the same socket is refused, and the refusal says which lock is
      held.
- [x] The original repro is fixed: 12 rounds × 8 simultaneous starts → **0** rounds with
      more than one daemon (was 1 in 12).
- [x] The lock is released by the kernel when the holder ends, however it ends — no pid
      file, no liveness probe, no timeout. `ExclusiveLock`'s own tests cover
      exclusivity, release-then-reacquire, and per-path independence.
- [x] One lock implementation, not two: the updater's `UpdateLock` (T-0070) and the
      daemon's instance lock are the same type, so the reasoning cannot diverge.
- [x] The lock outlives the `serve` future: `main` cancels `serve` on SIGTERM and keeps
      draining and snapshotting, so the guard lives on the `Daemon`, not as a local.
- [x] Regression test fails **before** the fix and passes after (mutation-checked, see
      evidence).

## Verification

```console
cargo test -p arreo-server --test single_instance      # 2 passed
cargo test -p arreo-core --lib lock                    # 3 passed
bash .loop/evidence/T-0071/race.sh                     # 0 double-daemon rounds
```

## Evidence

`.loop/evidence/T-0071/` — the race harness, its before/after output, and the mutation
transcript showing the regression test goes red when the lock is removed.

## Findings

- **A test for a race is usually a bad test.** A racing test would have passed ~11 times
  in 12 before this fix — a test that reports "fine" about a broken machine. The
  regression test therefore asserts the *rule* ("only the lock holder may serve this
  path") in a deterministic form: the test takes the lock itself and leaves the socket
  **absent**, which is exactly the state both racing daemons saw. Pre-fix, the daemon
  binds and serves; post-fix it refuses. The race harness remains as evidence, not as the
  test.
- **`Daemon::is_live` is still worth having.** The lock makes the sequence safe, but the
  probe is what produces the operator's message when a *live* daemon holds the socket. The
  two answer different questions.
- **T-0038 now starts from a real invariant.** The handoff's "old daemon keeps serving
  until the new one commits" needs exactly one daemon; before this, two could be running
  and the handoff would have had no way to tell which one to hand off to.
