---
id: T-0067
title: "Pinning has two answers — `devices list` and `machines trust` read the store, authorization reads the store + disk"
phase: 2
priority: 2
status: proposed
depends_on: []
scope:
  - crates/arreo-core/src/identity/authority.rs
  - crates/arreo-cli/src/machines.rs
  - crates/arreo-cli/tests/pairing.rs
  - .loop/evidence/T-0067/**
---

## Goal

"Is this device pinned on this machine?" has **two answers** depending on who asks:

- **Authorization** (the handshake, `SessionAuth`, the per-verb gate) asks the authority's *index*,
  which is built from the store's records **plus the certificates on disk** — `reload()` says so
  outright: *"A record with no cert file (or a cert file with no record) still counts as a pinned
  device as long as the certificate verifies."*
- **`arreo devices list` and `arreo machines trust`** ask `DeviceAuthority::devices()`, which reads
  **the store only**.

So a certificate that landed on disk without a store row is a device the daemon will authenticate
and the CLI will deny exists. The refusal even asserts the opposite of the truth.

## Reproduction (`.loop/evidence/T-0067/`)

One box, real binaries. The setup is ordinary: `arreo pair` runs against the *default* socket
(so its pin row goes to the default socket's store), while the daemon runs on an explicit
`--socket` (so its store is a different file) — both share one identity directory, which is where
certificates live.

```console
$ ARREO_IDENTITY_DIR=$D/a arreo devices list --socket $D/a/fresh.sock
root ff22022740d68645…
no devices paired yet (pair one, or issue from a public key)

$ ARREO_IDENTITY_DIR=$D/a arreo machines trust dev_09a94313c9eee98a1f9520f514060513 --yes --socket $D/a/fresh.sock
machines trust: dev_09a94313c9eee98a1f9520f514060513 is not pinned on this machine, so a grant
would do nothing (it could not authenticate here). Pin it first: `arreo devices issue --key …`
```

Both statements are false: `identity/devices/09a94313c9eee98a1f9520f514060513.cert` is on disk, it
verifies under this machine's root, and the authority's index — the thing the handshake consults —
includes it, which is why a daemon at that same socket authenticates the device fine.

## Acceptance criteria

- [ ] The question "is this device pinned here?" is answered by **one** function on
      `DeviceAuthority`, and both the listing and `machines trust`'s pre-flight use it. Whether that
      function reads the index, the store, or both is a decision to write down — with the argument,
      because it is a policy question and not just a refactor: *does a verified certificate on disk
      constitute a pin?*
- [ ] `arreo devices list` and `arreo machines trust` agree with authorization: a device the daemon
      would authenticate as pinned is listed, and is grantable.
- [ ] A refusal's message is true. `pinned_here`'s current sentence — "it could not authenticate
      here" — must not be printed for a device whose certificate verifies.
- [ ] A test covers the mismatched-socket shape that produced this: `pair` on the default socket, a
      daemon (or a listing) on an explicit one, asserting the device is visible and grantable. The
      third case is the one nobody tests today: the store *empty* and the cert dir *populated*.
- [ ] No regression for the ordinary case: `crates/arreo-server/tests/trust.rs` (local trust) and
      `crates/arreo-cli/tests/machines.rs` stay green, and T-0059's "refuses an unpinned device so a
      typo is caught" still refuses a device that genuinely has no certificate here.

## Notes

- **Re-scoped from a misdiagnosis, deliberately kept as the record.** This file was filed as
  "a self-admitted certificate does not verify" (p1). That report was **wrong**, and the way it was
  wrong is worth keeping:
  - the exit-criterion script used **fixed ports** and never killed its relay, so runs 2+ talked to a
    **stale relay** from run 1 — which held an account registered with the *secret* seed instead of
    the public key. The relay then rejected a perfectly good certificate, and the message
    ("certificate signature does not verify under the root key") read exactly like a product bug.
  - the script is now pid-scoped ports, `trap`-killed, poll-not-sleep, and registers the account from
    `devices list --json` (the human line truncates the key; `root.key` holds the secret).
  - with that fixed, the criterion passes: `.loop/evidence/T-0066/exit-criterion.sh` reaches two
    machines in one account in **1 s** (budget 300 s), both listed `online`.
  - the lesson is the harness one this repo keeps relearning: **a leaked process with a fixed address
    is indistinguishable from a product defect**, and the first thing to check when a security
    failure looks impossible is whether the peer is the one you think it is.
- This finding survived that correction: it reproduces on a clean box with no leaked processes.
  The reproduction above was taken after the stale relays were killed.
- Related: T-0059 (`machines trust`) owns `pinned_here`; T-0046 owns the trust model this sits inside.
  Neither is wrong about the *rules* — this is one fact with two sources, the defect class this
  project keeps finding.

## Verification

```console
cargo test -p arreo-cli --test pairing
cargo test -p arreo-cli --test machines
cargo test -p arreo-server --test trust
```
