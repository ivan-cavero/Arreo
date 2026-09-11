# ADR 0018 — The machine join handoff (`arreo machines add`)

Status: accepted (T-0058)
Context: ROADMAP §3.7 (the machine directory), ADR 0012 (machine directory rules),
T-0024 (SPAKE2 pairing), T-0056 (the directory over the wire), T-0044 (`arreo machines`)

## The problem

T-0044's `arreo machines add <pairing-code>` has to make a machine that has never
seen an account into a member of it: one command, on the machine being admitted,
using only what a human can carry between two machines.

Two facts constrain the design, and both were established by reading the code
rather than by preference:

1. **The relay authenticates a device by its certificate chain, and the chain
   ends at the account root.** `Router::handle_connection` reads the account's
   registered root key and calls `verify_auth(…, &root, …)`, which requires the
   presented certificate to verify under it. So a certificate a device can
   actually use has to be **issued by a machine that holds the account root
   key** — the key the first pairing machine owns.
2. **A machine's directory row is keyed by that machine's own root public key**
   (`MachineId::from_key`, T-0043/T-0056), and it is claimed by a signature the
   machine itself makes over the relay session's nonce. No other party can write
   that row, by construction.

Together they mean the handoff has two distinct halves: an **issuing** act by a
machine that already belongs, and a **self-registration** act by the machine
being admitted. Any design that collapses them asks one side to speak for the
other, and both facts forbid it.

## Decision

**The invite carries the account and the relay; the admitting machine issues;
the joining machine registers itself.**

- `arreo pair` on the admitting machine — which is the machine that holds the
  account root, because that is the machine that created or joined the account —
  extends its invite URI with `a=<account>` and `r=<relay host:port>` when its
  `[relay]` configuration enables one. Both are public metadata: an account id
  and an address. No key travels in the invite, and absent fields mean an
  ordinary pairing, exactly as before.
- `arreo machines add <pairing-code> --uri <invite>` runs **on the machine being
  admitted**. It runs the same SPAKE2 exchange `arreo pair --join` runs (one
  implementation, `join_pairing`), which yields a certificate chaining to the
  account root — signed by the machine that could sign it. It then connects to
  the relay named in the invite, asserts its **own** directory row with its
  **own** root key, and prints the granted name and its machine fingerprint.
- The invite is the only channel. That is not a limitation to work around: the
  joining machine has no configuration to read (it is joining *because* it has
  none), and the admitting machine is the only party that knows both the account
  and the relay.

The name is a request, as T-0043 defines it: a live name for another machine
comes back with the deterministic suffix, and the CLI reports the name that was
**granted**, not the one that was asked for.

## Rejected alternatives

**An account-wide trust list at the relay, where `add` writes a row for a key it
was handed.** Rejected: it lets any account device claim any name for any key
("I am `the-pi`" with a key nobody proved possession of), and it moves the
account's authority into the relay, which §3.7 deliberately keeps metadata-only.
Also rejected for the same reason in `docs/relay-protocol.md` §8.3's shape: a
relay that can admit machines is a relay that can be told to.

**The relay brokering the invite: a joining machine posts its invite to the
account's mailbox and `add <code>` looks it up, so no `--uri` is needed.**
Rejected: it adds a relay verb and makes the relay hold an **unauthenticated**
invite — anything that can reach the relay could post one, and anything that can
guess a four-word code could then claim it. The convenience (one fewer shell
argument) does not buy a new unauthenticated write path into the account.

**Auto-extend on first cross-machine attach: when A attaches to B, A's devices
are granted on B.** Rejected, and this is the convenience that would silently
void the model. It makes reachability a grant — the first thing that touches a
machine is trusted by it — which is exactly what §3.7's "a phone paired to the
VPS is *not* automatically paired to the Pi" exists to prevent. T-0046 lands the
explicit grant that this ADR deliberately does not automate.

**A device key instead of the machine's root key as the directory identity.**
Rejected: a device key is replaced when a device re-pairs (or is revoked), and a
directory row keyed by one would change identity exactly when the machine did not
change. The root key is the one key that outlives device churn, which is the
property a machine directory needs.

**`add` accepting a host/port instead of an invite.** Rejected: that is the
SSH-bookmark shape §3.7 exists to avoid, and it would need a way to authenticate
the far machine that is not a pinned key — a pinned key is what the invite
already carries.

## Consequences

- **The admitting machine must have a relay configured**, or its invite names no
  account and `add` refuses with exit 4 and a message saying so. That is a real
  requirement with a real failure mode, and it is stated rather than papered
  over: an operator who admits a machine without one gets a refusal, not a
  machine that silently joins nothing.
- **The joining machine ends up with two identities**, and they are different on
  purpose: a **device** key/certificate (issued by the account root — how the
  relay authenticates it) and a **root** key (`MachineId::from_key` — its
  directory identity). `add` must persist both; the row it writes is keyed by the
  second.
- **`add` is not idempotent against a used code.** A pair code is single-use
  (T-0024), so joining twice takes two admissions. Re-running `add` with a spent
  code fails at the exchange, before anything is written — the same
  persist-only-on-success rule `arreo pair --join` follows.
- **The certificate grants whatever role the admitting machine chose**, which is
  the account's door, not the machine's. What the joining device may do *to this
  machine* is T-0046's per-machine grant, and this ADR deliberately does not
  pre-empt it.

## Honest gaps

- The invite is carried by a human (a shell argument or a paste). It is not a
  secret — the code is, and the invite alone cannot complete a pairing — but a
  mistyped relay address is only caught when the connection fails, with a message
  about a connection rather than about the invite.
- Nothing expires the `a`/`r` fields beyond the invite's own TTL: an invite
  pasted into a chat window names an account and a relay for as long as the
  message lives. Those are public metadata, and the code that makes them usable
  dies with the pairing session.
- There is no way to admit a machine without a human on both ends. That is
  deliberate, and it is the same shape as the pairing it reuses.
