---
id: T-0064
title: A remote trust refusal at Hello hangs the client — no timeout on the post-Hello read
phase: 2
priority: 2
status: proposed
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

- [ ] The post-Hello read is bounded: a peer that accepts the stream and then answers
      nothing fails with a message naming the peer and the wait, rather than hanging. The
      bound belongs with the other handshake bounds (`REMOTE_HANDSHAKE_TIMEOUT`), not as a
      new constant with a different rationale.
- [ ] A refusal that *is* delivered still surfaces verbatim and still maps to the trust exit
      code (5) — the fix must not turn a working refusal into a timeout. Asserted by a test
      that observes the refusal text and the code, not merely that the call returned.
- [ ] The same bound covers the local-socket path: a daemon that dies between accept and
      Welcome must not hang a CLI either. (Same defect, second transport — the client is one
      implementation for both, which is why the fix is one place.)
- [ ] T-0047's mesh slice check "an untrusted device is refused with the actionable message"
      flips from SKIP to PASS, and the skip is removed rather than left as history.
- [ ] A regression test proves the bound: with a peer that reads Hello and answers nothing,
      the call returns an error inside the bound. (Constructed against a socket the test
      controls — no relay needed for this half.)

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
