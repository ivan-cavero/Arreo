---
id: T-0025
title: Device identity — per-device ed25519 keypair, server-signed certificate, pinning
phase: 2
priority: 1
status: done
depends_on: [T-0013, T-0018]
scope:
  - crates/arreo-core/src/identity/**
  - crates/arreo-core/tests/identity.rs
  - crates/arreo-core/src/store.rs
  - crates/arreo-core/Cargo.toml
  - crates/arreo-server/src/devices.rs
  - crates/arreo-server/tests/devices.rs
  - crates/arreo-cli/src/main.rs
  - crates/arreo-cli/tests/devices.rs
  - crates/arreo-tui/Cargo.toml
  - supply-chain/**
  - specs/adr/0009-device-identity.md
---

## Goal

Every client is a *device* with its own ed25519 keypair and a certificate signed by the
server's root key; the certificate is the pin, so authorization means "this exact device",
not "whoever holds a copy of a config" (§3.3, §4). Keystone of the Phase 2 security slice:
pairing (T-0024) issues it, the Noise-KK transport (T-0023) consumes it, revocation (T-0026)
kills it. v1 roles per §4: owner + viewer.

## Acceptance criteria

- [x] Server root keypair generated on first daemon start; a `DeviceCert` binds device id
      (fingerprint of the device public key), name, role, issued-at and serial, signed by
      the root key. Versioned fixed struct over the existing msgpack codec — not X.509.
      ADR 0009; `identity::{keys,cert}`; boot line `device authority ready (root …)`.
      Identity: `sha256(pubkey)[..16]` as `dev_<hex32>`, re-derived from the key on every
      verification, so a cert cannot name a device whose key it does not carry.
- [x] Verification is total: malformed, truncated, wrong-version or foreign-signed certs
      give typed errors, never a panic (proptest over arbitrary bytes plus a committed
      hostile-cert corpus).
      `tests/identity.rs`: 9-case hostile corpus, a signature-length case (0/1/32/63/65/128
      bytes must not decode), proptests over arbitrary bytes and arbitrary signature bytes.
- [x] A device without a cert cannot open a remote session: the pinned-key check refuses an
      unknown key before any policy is consulted, the refusal is typed, and an `auth_reject`
      audit row is written. Proven across real processes (`arreo-cli/tests/devices.rs`).
      The transport wiring itself (Noise-KK static key ↔ pinned cert) lands with T-0023,
      which calls `DeviceAuthority::authorize` — the decision function this task ships and
      tests. `check_verb` additionally enforces the role in the same call.
- [x] Rotating a device key invalidates the old cert: rotation pins the new certificate,
      retires the old id durably (store column, survives a restart), and the old key's next
      connection fails with a typed refusal that **names the replacement device**
      (`CertError::RotatedAway`). Both events are `device_change` audit rows.
      Note: because a device id *is* its key fingerprint, "the old public key fails with
      `CertMismatch`" would be vacuous — a new key is a new id, so the refusal is
      `RotatedAway` when the authority knows the history and `NoCert` when it does not.
- [x] Storage is explicit and tested: server private material under
      `$XDG_DATA_HOME/arreo/identity/` (0700 dir, 0600 files: `root.key`, `devices/*.cert`),
      client keypair in the client's own dir, and **no private key in SQLite**: migration
      v3's `devices` table holds only public material (public key, role, serial, issued-at,
      last-seen, revoked, retired-to). Asserted by reading the raw database bytes and by
      mode checks; a loose (0644) key file is refused rather than trusted.
- [x] `arreo devices` lists id, name, role, issued-at, last-seen, status (`--json`), and
      roles bite: a viewer may attach/read/wait, only an operator may send or spawn (§4).
      The `authorize --verb <v>` form runs the transport's own decision path
      (`check_verb` = authenticate + enforce) and exits non-zero on refusal.
- [x] Evidence under `.loop/evidence/T-0025/`: cert + rotation transcripts, hostile-cert
      results, `arreo devices` output, clean redaction scan.

## Evidence

- `.loop/evidence/T-0025/device-lifecycle.txt` — a real CLI session: identity created,
  owner issued and authorized, viewer allowed to read and denied `send`, rotation
  replacing the key (old key refused with the replacement named), revocation, and the
  `auth_reject`/`device_change` audit rows.
- `.loop/evidence/T-0025/daemon-boot.txt` — bootstrap (root key 0600 in a 0700 dir,
  stable across restarts) and the refusal path (an unusable root key exits non-zero and
  is **not** replaced).
- Tests: `crates/arreo-core/src/identity/**` (unit), `crates/arreo-core/tests/identity.rs`
  (hostile corpus + proptest), `crates/arreo-cli/tests/devices.rs` (process-level lifecycle),
  `crates/arreo-server/tests/devices.rs` (boot behavior).
- ADR 0009 records the design (why not X.509, why secrets never reach the store, why
  rotation is explicit).

## Notes

- `ed25519-dalek` 2.x (pure Rust, no OpenSSL) — §3.3 already decided ed25519 device
  identity; `rand_core`/`getrandom` for keygen, `zeroize` for key material. Cert encoding
  reuses `serde` + `rmp-serde`, riding the same audited codec as the wire protocol.
- Rejected: X.509/PKI — CA semantics we do not need plus a parser class to fuzz for zero
  product value; the point is "devices, not CAs" (§3.2). Rejected: private keys in the DB —
  the DB gets copied, backed up and synced, so key material there is a leak surface.
- Rejected for now: `keyring` for the *server* root key — headless daemons and systemd user
  units have no unlocked keychain at boot. §4's keychain decision applies to client secrets
  (Secure Enclave/Keystore, Phase 3); keychain-wrapping the device key at rest is a
  follow-up, and the server root key stays file-permission protected.
- The mesh's per-machine trust record (§3.7) builds on these types in a sibling task: it
  consumes `arreo_core::identity::{DeviceId, DeviceCert, DeviceStore}` and does not
  redefine them.
- Honest gap: no TPM/Secure Enclave-backed server root key, and a compromised server kernel
  is explicitly outside the v1 threat model (§4).

## Verification

```console
cargo test -p arreo-core identity
cargo test -p arreo-cli --test devices
cargo test -p arreo-server --test devices
cargo xtask e2e --slice persistence
```

Last run: 173 unit/integration tests green workspace-wide (2026-09-11).

The end-to-end device lifecycle proof (real binaries, cert issued, cert refused) lands in
T-0027's `--slice pairing`.
