---
id: T-0126
title: A handed-over daemon cannot push — it has no relay session
phase: 3
priority: 3
status: proposed
depends_on: [T-0117, T-0105]
scope:
  - crates/arreo-server/src/main.rs
  - crates/arreo-server/src/handoff.rs
  - crates/arreo-server/src/relay_client.rs
  - crates/arreo-server/tests/notify_push.rs
  - docs/notifications.md
  - .loop/evidence/T-0126/**
verify:
  - cargo test --workspace
  - cargo test -p arreo-server --test notify_push
---

## Why this exists

Found by the T-0117 worker and recorded rather than fixed, because the fix is outside that task's
fence: **a daemon that takes over a live handoff has no push leg.**

`arreo update --server` (T-0105 / T-0038) replaces the daemon by handing the listener, the lock
and the panes to an incoming process. That process is built by `Daemon::new` and has no relay
session of its own yet — the relay session is started by `main.rs`'s composition root, and the
handoff path returns before it. So on a handed-over daemon, notifications are written to the audit
log and **never pushed**: the operator's phone silently stops being told anything, with no error,
until the daemon is restarted outside a handoff.

T-0117 made that visible rather than silent — the daemon logs "notifications are on but no push
leg is wired" — which is the honest minimum. This task makes it work.

## Why it matters more than it looks

The failure is invisible in the worst way: the notification **row** is written, so `arreo notify
--why` answers, the audit trail is complete, and every test that checks the daemon's own state
passes. Only the phone is silent, and only if the operator happened to have been told before the
handoff. A push that stops after an update is exactly the class of defect the phase's own
"offline-queued delivery" criterion exists to prevent, one layer up.

## Acceptance criteria

- [ ] The incoming daemon of a handoff has a push leg: a relay session is established (or adopted)
      so a delivered notification reaches the paired devices after the cut, not only before it.
- [ ] The handoff's own constraints are respected: the incoming daemon must not dial twice, must
      not steal the outgoing daemon's relay registration (T-0060's one-session-per-device rule —
      the relay refuses a second registration for the same device, so the sequencing matters), and
      must not delay readiness past the handoff's budget.
- [ ] **The window is stated**: how long after the cut a notification may be row-only. If there is
      an unavoidable gap, it is measured and bounded rather than undefined, and the log line
      T-0117 added stays for the case where a leg genuinely is absent.
- [ ] A test proves it across a real cut: a notification delivered *after* a handoff reaches a
      paired device. Extend `crates/arreo-server/tests/notify_push.rs` (it already spawns a real
      relay, a machine and a phone) rather than starting a new harness.
- [ ] `docs/notifications.md` records the behaviour across a handoff.

## Notes

- The same question applies to any future background producer that needs the relay session
  (metrics rollups, presence beats), so the shape worth aiming for is "the daemon owns a relay
  session handle from whichever start path built it" rather than a handoff-specific patch.
- Do not simply restart the relay session in the incoming daemon without checking T-0060: a
  second registration for one device is refused by the relay, and the outgoing daemon's session
  may still be live for a moment after the cut.
