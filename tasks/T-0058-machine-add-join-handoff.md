---
id: T-0058
title: "`arreo machines add` — the join handoff from a pairing code"
phase: 2
priority: 2
status: proposed
depends_on: [T-0024, T-0043, T-0056]
scope:
  - crates/arreo-cli/src/machines.rs
  - crates/arreo-cli/src/main.rs
  - crates/arreo-core/src/pairing/**
  - crates/arreo-cli/tests/machines.rs
  - docs/machines.md
  - specs/adr/0018-machine-join-handoff.md
  - .loop/evidence/T-0058/**
---

## Goal

T-0044's `add <pairing-code>` criterion cannot be met today, and this task is why: the join is
per-machine (T-0056 — the row is claimed by a *signature the machine makes over its own key*), and the
pairing code carries neither the machine's identity nor the account's coordinates. **This task decides
the handoff and then implements it**, rather than inventing a format inside a CLI task.

## The decision this task has to make (pick one, record it in the ADR)

1. **The invite carries the account.** `arreo pair` on the joining machine prints an invite extended
   with the account id and relay address; the operator runs `arreo machines add "<code>" --uri
   <invite>` on a machine that already belongs, the exchange pins the joining device *to the admitting
   machine*, and the joining machine's daemon then asserts its own row (T-0056's job, already built).
   The code stays a four-word phrase; the URI stays product-shaped (never an arbitrary host/port).
2. **The relay brokers the invite.** A joining machine posts its invite to the relay mailbox for its
   account; `arreo machines add <code>` looks it up by code, so no `--uri` is needed and the code
   alone suffices — at the cost of a new relay verb and of the relay holding an unauthenticated
   invite.
3. **`add` is the server side.** The joining machine displays the code, the admitting operator types
   it, and the admitting side issues the joining device's certificate; the joining machine then
   asserts its own row. This is `arreo pair`'s existing direction with the account coordinates
   supplied by the invite the *admitting* side already knows.

## Acceptance criteria

- [ ] The ADR records the choice and the rejected alternatives, including why the code alone cannot
      admit anyone today (a proof of possession by the machine's key is required to write a row, and a
      four-word phrase authenticates a *pairing session*, not a machine).
- [ ] `arreo machines add <pairing-code> [--name <name>]` completes the join: it uses the single-use,
      five-minute pairing session (never a long-lived secret), takes the name from `--name` or the
      joining side, and prints the granted name (plain or T-0043's deterministic suffix) plus the
      machine fingerprint — read back from the directory, never assumed.
- [ ] It never accepts an arbitrary host/port (the invite is the product's own value, and a bare
      address in that position is refused with the reason), and it never silently renames an existing
      machine.
- [ ] A refused join (expired code, a code for another account, a name the directory rejects) changes
      nothing: a second read of the directory shows the same rows, asserted by a test that reads
      before and after.
- [ ] `docs/machines.md` loses the "not implemented yet" note for `add`, and the `--help` text with it.
- [ ] Evidence `.loop/evidence/T-0058/`: a real two-machine transcript (invite, add, granted name with
      a suffix because the plain name was live, and the joining machine seen by a third reader).

## Notes

- The joining machine still ends up asserting its own row: T-0056 makes the row keyed by a key that
  machine holds, and no amount of admitting-side cooperation can substitute for that signature. The
  handoff's job is to tell the joining machine *which account and relay* it is joining, and to let the
  operator authorize it — which is also where T-0046's per-machine trust will hang its default grant.
- Rejected: `add` taking a machine id and writing a row for a key nobody proved (any account device
  could then claim any name for any key), and `add` taking an address (that is the SSH-bookmark shape
  §3.7 exists to avoid).

## Verification

```console
cargo test -p arreo-cli --test machines
cargo test -p arreo-core --lib pairing
```
