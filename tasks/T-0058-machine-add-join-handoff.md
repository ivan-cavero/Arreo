---
id: T-0058
title: "`arreo machines add` — the join handoff from a pairing code"
phase: 2
priority: 2
status: done
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

- [x] The ADR records the choice and the rejected alternatives, including why the code alone cannot
      admit anyone today (a proof of possession by the machine's key is required to write a row, and a
      four-word phrase authenticates a *pairing session*, not a machine).
- [x] `arreo machines add <pairing-code> [--name <name>]` completes the join: it uses the single-use,
      five-minute pairing session (never a long-lived secret), takes the name from `--name` or the
      joining side, and prints the granted name (plain or T-0043's deterministic suffix) plus the
      machine fingerprint — read back from the directory, never assumed.
- [x] It never accepts an arbitrary host/port (the invite is the product's own value, and a bare
      address in that position is refused with the reason), and it never silently renames an existing
      machine.
- [x] A refused join (expired code, a code for another account, a name the directory rejects) changes
      nothing: a second read of the directory shows the same rows, asserted by a test that reads
      before and after.
- [x] `docs/machines.md` loses the "not implemented yet" note for `add`, and the `--help` text with it.
- [x] Evidence `.loop/evidence/T-0058/`: a real two-machine transcript (invite, add, granted name with
      a suffix because the plain name was live, and the joining machine seen by a third reader).

## Notes

- The joining machine still ends up asserting its own row: T-0056 makes the row keyed by a key that
  machine holds, and no amount of admitting-side cooperation can substitute for that signature. The
  handoff's job is to tell the joining machine *which account and relay* it is joining, and to let the
  operator authorize it — which is also where T-0046's per-machine trust will hang its default grant.
- Rejected: `add` taking a machine id and writing a row for a key nobody proved (any account device
  could then claim any name for any key), and `add` taking an address (that is the SSH-bookmark shape
  §3.7 exists to avoid).

## Landing notes (2026-09-11)

**Option 1 was the only option the code allows**, and confirming that took reading
`Router::handle_connection` rather than reasoning from the roadmap: the relay verifies a device
certificate against the **account root** (`verify_auth(…, &root, …)`), so the certificate a joining
machine can actually use must be issued by a machine that *holds* the account root — which is the
machine that admits it, and which `arreo pair` already is. Meanwhile T-0056 makes the directory row
keyed by the joining machine's **own** root key and claims it with a signature only that machine can
produce. So the handoff is necessarily two acts by two machines, and the design is the smallest thing
that connects them: the invite carries the account and the relay; the admitting machine issues; the
joining machine registers itself. ADR 0018 records this and the four alternatives rejected.

**`add` runs on the joining machine**, which is worth stating because the wording of the original
criterion could be read either way. It has to: the joining machine is the one with no configuration,
so the account and relay must arrive in the invite, and the only party that knows both is the machine
that already belongs.

**The phone half was extracted, not copied.** `arreo pair --join` and `arreo machines add` run the
same SPAKE2 exchange; `add` just keeps going afterwards. Two copies of that flow would be two places
for the persist-only-on-success rule (and the key-reuse rule, and the pinned-server rule) to drift, in
security-critical code where a drift is a hole. `join_pairing` is now the one implementation, and
`crates/arreo-cli/tests/pairing.rs` still passes untouched.

**The invite's new fields are additive.** `v` stays 1, `a`/`r` are optional, and a URI without them is
exactly what every pre-T-0058 invite was — so an old invite still pairs, and `add` on one refuses with
"names no account" rather than doing something surprising. A *half*-filled pair is refused at parse
time: a joining machine sent looking for an account on a relay it was not told about would otherwise
fail much later with a message about a connection.

**A flag belongs to one verb or none.** Moving into `add` meant the shared parser grew `--name` and
`--uri`, which the other verbs must not silently accept: `refuse_unused` makes every verb that does not
read them an error, so `machines rename --name x` is a usage error rather than a rename that quietly
ignored a flag the caller believed in.

**Found while testing:** the first version of the `add` tests passed a machine's `--uri` through the
*config* path and quietly exercised the wrong branch. The tests now assert the invite URI contains
`&a=acct-1&r=<relay>` before using it, which is what caught the hint being dropped (proved by removing
`directory_hint`'s return value and watching the test go red).

**Honest gaps:** `add` is not idempotent against a spent code (pairing codes are single-use, T-0024),
so re-joining takes a new admission; a machine admitted while the relay is unreachable keeps its
certificate and is told so, with no retry loop; and `specs/**` is still outside `REUSE.toml`'s
coverage — pre-existing (0012–0017 are in the same position) and T-0048's `reuse lint` is where it
gets fixed, so this task did not widen its fence to touch licensing.

## Verification

```console
cargo test -p arreo-cli --test machines
cargo test -p arreo-core --lib pairing
```
