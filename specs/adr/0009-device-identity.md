# ADR 0009: device identity — self-signed certs, roles, and where secrets live

- Status: accepted (2026-09-11, T-0025)
- Context: Phase 2 makes Arreo reachable from other machines (§3.2, §4), which
  means the daemon must answer "who is this, and what may they do" without a
  human present. §3.3 already decided *devices*: each client holds its own
  ed25519 keypair, and the server pins it. What remained open was the shape of
  the credential, where the secrets live, and where the decision is made.
- Decision: a `DeviceCert` is a **fixed, versioned struct signed by the server's
  root key**, encoded with the same MessagePack codec as the wire protocol —
  deliberately not X.509. A device's identity *is* the fingerprint of its public
  key (`sha256(pubkey)[..16]`, `dev_<hex32>`), so a certificate can only name a
  device whose key it carries: a cert that verifies for a different key is not a
  weaker credential, it is a different device. The root secret lives in
  `$XDG_DATA_HOME/arreo/identity/root.key` (0600, dir 0700); device certs live in
  `identity/devices/<id>.cert`; the SQLite store (schema v3) holds **public
  material only** plus `revoked`/`retired_to`/`last_seen_ms`. Roles are v1's two
  (`owner`, `viewer`) and every socket verb maps to exactly one capability in
  `identity::role::required` — an exhaustive match, so a new verb without a
  policy decision does not compile. One call, `DeviceAuthority::check_verb`,
  authenticates *and* enforces the role; every refusal writes an `auth_reject`
  audit row before returning. The authority lives in `arreo-core` (the CLI's
  `arreo devices …` operates on exactly the same files and store the daemon
  does); `arreo-server::devices` is the wiring, because nothing but `xtask` may
  depend on `arreo-server` (T-0001).
- Why this one:
  - **No X.509.** The model is devices, not a CA: one root key signs a flat list.
    X.509 would add chain building, path validation and a parser class to fuzz
    for zero product value, and its failure modes (name constraints, extensions)
    are exactly the ones we would have to audit for nothing.
  - **The id is derived, not asserted.** `DeviceId::from_key` means an attacker
    cannot present a valid cert for someone else's device id: the payload's id is
    re-derived from the key and compared before the signature is even checked, so
    the error names the real problem.
  - **The store cannot escalate.** Records loaded from SQLite are only accepted
    after their certificate verifies, and the *verified certificate* supplies the
    role, name and serial — the row contributes only facts a certificate cannot
    carry. A tampered database file therefore cannot promote a viewer to owner
    (asserted in `devices.rs`).
  - **Rotation is explicit.** Because the id is the key fingerprint, a new key is
    a new id; without an explicit rotate the two would coexist and "rotation"
    would silently be "a second device". `rotate` pins the new cert, retires the
    old id durably, and the old key's next connection is refused with
    `RotatedAway { replaced_by }` — naming where the device went, which is the
    difference between a user fixing their config and a user filing a bug.
  - **Refusals are events.** An audit log that only records successes answers the
    wrong question after an incident.
- Alternatives rejected:
  - **Certificates over JSON**: the project already has one audited codec
    (ADR 0006); a second encoding is a second parser to fuzz.
  - **Private keys in SQLite** (or a password-encrypted key in the same file):
    the database is the thing that gets copied, backed up and synced.
  - **`keyring` for the server root at v1**: headless daemons and systemd user
    units have no unlocked keychain at boot; file permissions are the honest
    control until the Secure Enclave/Keystore work lands for clients (Phase 3).
  - **Global trust for the mesh**: rejected in favour of per-machine grants
    (T-0046) — a device trusted on the VPS is not ipso facto trusted on the Pi.
- Consequences: `DeviceIndex` is the pinned set; `DeviceAuthority::authorize` is
  what T-0023's transport calls for an incoming peer, and `check_verb` is the
  policy half. Store schema is v3 (v2 databases migrate in place, `audit.kind`
  added with existing rows defaulting to `prompt`). The pairing flow (T-0024)
  issues through `DeviceAuthority::issue`, so there is exactly one code path that
  mints a certificate. Weak (small-order) public keys are refused at pin time —
  the crypto layer will *encode* what it is given, and the authority is where
  that becomes policy.
