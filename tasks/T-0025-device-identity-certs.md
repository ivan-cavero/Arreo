---
id: T-0025
title: Device identity — per-device ed25519 keypair, server-signed certificate, pinning
phase: 2
priority: 1
status: proposed
depends_on: [T-0013, T-0018]
scope:
  - crates/arreo-core/src/identity/**
  - crates/arreo-core/src/store.rs
  - crates/arreo-core/Cargo.toml
  - crates/arreo-server/src/devices.rs
  - crates/arreo-cli/src/main.rs
---

## Goal

Every client is a *device* with its own ed25519 keypair and a certificate signed by the
server's root key; the certificate is the pin, so authorization means "this exact device",
not "whoever holds a copy of a config" (§3.3, §4). Keystone of the Phase 2 security slice:
pairing (T-0024) issues it, the Noise-KK transport (T-0023) consumes it, revocation (T-0026)
kills it. v1 roles per §4: owner + viewer.

## Acceptance criteria

- [ ] Server root keypair generated on first daemon start; a `DeviceCert` binds device id
      (fingerprint of the device public key), name, role, issued-at and serial, signed by
      the root key. Versioned fixed struct over the existing msgpack codec — not X.509.
- [ ] Verification is total: malformed, truncated, wrong-version or foreign-signed certs
      give typed errors, never a panic (proptest over arbitrary bytes plus a committed
      hostile-cert corpus).
- [ ] A device without a cert cannot open a remote session: the Noise-KK static key must
      match a pinned cert; an unknown key is refused before any `Message` is accepted, the
      client gets a typed `Error`, the socket closes, and an `auth_reject` audit row is
      written. Proven with a real second process holding a fresh keypair.
- [ ] Rotating a device key invalidates the old cert: rotation writes a new keypair + cert;
      the old public key's next connection fails with a typed `CertMismatch`, and the old
      private key can never open a session again. Both events are audit rows.
- [ ] Storage is explicit and tested: server private material under
      `$XDG_DATA_HOME/arreo/identity/` (0700 dir, 0600 files: `root.key`, `devices/*.cert`),
      client keypair in the client's own dir, and **no private key in SQLite**: migration
      v3's `devices` table (public key, cert, role, issued-at, last-seen) holds none.
- [ ] `arreo devices` lists id, name, role, issued-at, last-seen, status (`--json`), and
      roles bite: a viewer may attach/read/wait, only an operator may send or spawn (§4).
- [ ] Evidence under `.loop/evidence/T-0025/`: cert + rotation transcripts, hostile-cert
      results, `arreo devices` output, clean redaction scan.

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
cargo test -p arreo-server devices
cargo xtask e2e --slice persistence
```

The end-to-end device lifecycle proof (real binaries, cert issued, cert refused) lands in
T-0027's `--slice pairing`.
