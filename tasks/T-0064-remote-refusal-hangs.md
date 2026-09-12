---
id: T-0064
title: A remote trust refusal at Hello hangs the client — no timeout on the post-Hello read
phase: 2
priority: 2
status: done
depends_on: [T-0046]
scope:
  - crates/arreo-core/src/mesh/session.rs
  - crates/arreo-cli/src/remote.rs
  - crates/arreo-cli/tests/remote_machine.rs
  - .loop/evidence/T-0064/**
---

## Goal

T-0046's promise is that a device this machine has pinned but never granted is **refused with
the actionable message** — and the daemon does exactly that: it answers Hello with the
refusal, names the command that fixes it, and flushes before closing (`serve_session`'s
wrapper, T-0052's fix). What does not work is the other end: the client's read after Hello
has **no timeout**, so when the refusal does not arrive the command hangs forever instead of
reporting anything.

Found by T-0047's mesh slice, which asserts that exact sentence and instead timed out with
**zero output** at 60 s — the worst possible failure shape, because a hang says nothing about
what went wrong.

## Evidence (from T-0047's slice, 2026-09-12)

Beta's daemon log, for a device that holds a valid account certificate, is pinned on beta,
and has had its grant cut:

```
arreo-server: relay peer dev_67a590f2ba2867cf3b87d4c77156632f authenticated
daemon: refusing Hello for dev_67a590f2ba2867cf3b87d4c77156632f on this machine: machine
  vmi3525064 revoked this device's grant, so Hello is refused. Re-grant it with:
  arreo machines trust dev_67a590f2ba2867cf3b87d4c77156632f --machine vmi3525064 --role
  viewer --yes
```

So the refusal is produced, correct, and actionable. The client (`arreo panes --machine
<name>` from a machine with no daemon) printed **nothing** and never exited — killed at 60 s.

## Acceptance criteria

- [x] The post-Hello read is bounded by `HANDSHAKE_REPLY_TIMEOUT` (5 s), beside the other
      handshake bounds, and a silent peer now fails with a message naming the wait. **Five
      seconds, not ten, because of the budget**: this bound runs *after* the Noise handshake,
      so the worst case an operator hits is that handshake's bound plus this one, and §5's row
      is 10 s — ten here would spend the whole budget on the last step.
- [~] **Moved to T-0065, because fixing the bound proved the refusal is not delivered at all.**
      The daemon writes the right sentence (its own log has it, with the grant command) and the
      frame is lost on the relay path — so there is no working refusal for this task to keep
      working. T-0064's fix is what made that visible: a hang became a named failure. The mesh
      slice asserts the bounded failure as a PASS and the missing sentence as a SKIP naming
      T-0065.
- [x] The same bound covers the local-socket path: the fix is in `Client::connect_to`, which
      both transports go through, so a local daemon that accepts and answers nothing fails the
      same way. (Same defect, second transport — one implementation for both, which is why the
      fix is one place.)
- [x] T-0047's mesh slice no longer skips on the hang: the check is split into the bounded
      failure (PASS — this task's fix) and the missing sentence (SKIP naming T-0065). The old
      blanket skip is gone.
- [x] A regression test proves the bound: `mesh::session::tests::a_peer_that_answers_nothing_fails_instead_of_hanging`
      binds a socket that reads the Hello frame and answers nothing, and asserts the call fails
      inside the bound. **Proved load-bearing by mutation**: with the timeout removed the test
      hangs past 60 s; with it, 5.00 s.

## Notes

- **Why this is not fixed inside T-0047:** the slice's fence is `xtask/**`; this is client
  code. The slice found it, which is what a referee is for.
- The reason this survived T-0046's tests: they exercise the trust gate over a **local
  socket**, where the refusal is written into a duplex the same process owns and always
  arrives. The remote path adds a relay, a Noise channel and a second process, and only the
  mesh slice exercises it.
- Severity: the refusal is the *only* thing the operator gets. A hang is strictly worse than
  a wrong message, because the wrong message can be read.

## Verification

```console
cargo test -p arreo-core --lib mesh::session
cargo test -p arreo-cli --test remote_machine
cargo xtask e2e --slice mesh
```

## Outcome

Fixed and pushed. `Client::connect_to`'s post-Hello read is bounded by
`HANDSHAKE_REPLY_TIMEOUT` (5 s), so a peer that accepts a connection and then says nothing
produces a named failure instead of a hang — on both transports, because both go through this
one function.

The finding that came out of it is T-0065: the daemon's refusal frame is written, flushed, and
never arrives over the relay. T-0064's fix is what made that visible, which is the useful
shape of a fix for a hang — the next failure is a sentence rather than a silence.
