---
id: T-0069
title: A refused registration burns the relay's handshake budget, replacing its own reason with a transport error
phase: 2
priority: 3
status: done
depends_on: [T-0051]
scope:
  - crates/arreo-server/src/relay_client.rs
  - crates/arreo-server/tests/relay_client.rs
  - .loop/evidence/T-0069/**
---

## Goal

A daemon whose relay registration is **refused** (no such account, a certificate this
account does not accept) retried on the transport's reconnect ramp — 250 ms, 500 ms,
1 s… — and the relay's handshake budget is **per address** (3 per 10 s). Within seconds
the daemon exhausted the budget for its whole host, after which every attempt was
refused *at the transport*, and the operator's log stopped showing the reason:

```
arreo-server: relay registration failed: … certificate signature does not verify under the root key   ← the truth
arreo-server: relay registration failed: … certificate signature does not verify under the root key
arreo-server: relay registration failed: … certificate signature does not verify under the root key
arreo-server: relay registration failed: … aborted by peer: the server refused to accept a new connection   ← the mask
```

The reason the operator needs is in the *first* line, and the lines that follow bury it.
On loopback — or any shared/NAT'd address — the same exhaustion also refuses **other**
devices that had nothing to do with the failure.

## What was done

A refusal is deterministic in everything the daemon controls, so it is retried at the
policy's **ceiling** rather than on its ramp: `retry_delay(refused, attempt, jitter)`.
The distinction is typed, not matched on text — `SessionError::Client(ClientError::Refused
{ .. })` is the relay having answered and said no, and everything else is a transport that
is not there.

It is still retried, at 30 s, because an operator may register the account or grant trust
while the daemon runs: waiting has to end by itself. And the log now says why it is
waiting, so the refusal stays the last thing a reader sees.

## Evidence (`.loop/evidence/T-0069/`)

Before — three refusals in the window, then the reason gone (the cascade above; observed
against a real relay with a real unregistered account).

After — one attempt, and the whole story:

```
arreo-server: relay registration failed (…): relay client: the relay refused the session:
  unknown account never-registered: this relay has no such account. Register it on the
  relay's host with `arreo-relay account add --account never-registered --root-key <the
  account's root PUBLIC key>` — a machine prints its own with `arreo devices list`
arreo-server: the relay refused this registration; that reason will not change by
  retrying, so the next attempt waits 30s (register the account, or fix the certificate,
  and it will connect)
arreo-server: retrying the relay in 34.525s
```

The relay's log shows a single refusal and **no** budget exhaustion, which is the other half
of the fix: a failing daemon no longer degrades the relay for everyone behind its address.

## Acceptance criteria

- [x] A refused registration does not retry on the transport ramp, and a transport failure
      still does. Asserted from outside, as `backoff_delay` already is (both are pure and
      total for exactly this reason): `a_refused_registration_waits_at_the_ceiling_not_on_the_ramp`
      in `crates/arreo-server/tests/relay_client.rs`, including the property that matters
      most — the **first** retry after a refusal already waits the ceiling.
- [x] The distinction is typed (`ClientError::Refused`), not a substring match on a message.
- [x] The reason survives in the log: the daemon explains the long wait instead of repeating
      the failure into a transport error.
- [x] Verified end to end against a real relay and a real unregistered account: one attempt
      in 30 s, one relay-side refusal, no budget exhaustion.

## Notes

- **Found by dogfooding the exit criterion** (T-0066): the unregistered-account path is what
  a new installation hits first, and the cascade is what it looked like before the fix.
- The severity is diagnostics, not security: the first log line was always honest, and the
  limiter's behaviour is correct in isolation (a refused attempt is not counted; the budget
  is spent by attempts that *reach* the Hello stage). What was wrong was the client's retry
  cadence choosing to spend the budget on an answer that could not change.
- The same reasoning argues for revisiting the budget itself someday: 3 per 10 s per
  **address** is tight for a NAT'd fleet, where many machines share one address and each
  dials at boot. `forgive` on a completed handshake keeps that benign in normal operation,
  and a refusal is now slow — but a burst of genuinely new machines could still trip it.
  Recorded here rather than acted on: changing a security parameter deserves its own
  rationale, not a drive-by.

## Verification

```console
cargo test -p arreo-server --test relay_client
```
