---
id: T-0067
title: A self-admitted certificate does not verify — `arreo pair` issues one the root cannot check
phase: 2
priority: 1
status: proposed
depends_on: []
scope:
  - crates/arreo-cli/src/main.rs
  - crates/arreo-core/src/identity/authority.rs
  - crates/arreo-core/src/pairing/**
  - .loop/evidence/T-0067/**
---

## Goal

`arreo pair` on a machine that holds the account root issues **itself** a certificate — the
documented-as-missing (T-0066) but working-in-principle bootstrap for the first machine. The
certificate is written, and then **nothing can verify it**: not the machine's own authority, not the
relay. Verified root cause is *not* the root key: the relay's stored account root is byte-identical to
this machine's root public key.

## Reproduction (`.loop/evidence/T-0067/`)

Minimal, from empty directories, on one box:

1. `arreo devices list --json` on a fresh identity dir → `root` = R (this machine's root public).
2. `arreo-relay account add --state-dir … --account … --root-key R` → accepted.
3. `arreo pair --mailbox … --config …` → prints four words; then
   `arreo pair --join "<those words>" --uri "<its own invite>"` → reports
   `paired with this server as viewer (dev_…)` and writes `identity/devices/<id>.cert`.
4. `arreo devices list --socket …` → **"no devices paired yet"**, although two certificates are
   present in `identity/devices/`.
5. Starting the daemon: `relay registration failed: the peer's certificate does not authorize it:
   certificate signature does not verify under the root key` — and it retries forever.

Cross-check that rules out the obvious explanation:

```console
$ sqlite3 relay.db "SELECT root_key FROM account"
b7982e04edd07dc116f300e2c6cf248bb824a7570f841e7cd5b219f70374681a
$ ARREO_IDENTITY_DIR=… arreo devices list --json | jq -r .root
b7982e04edd07dc116f300e2c6cf248bb824a7570f841e7cd5b219f70374681a
```

Identical. So the certificate is not signed by the key the account registered, and not by the key the
authority loads — which points at the **issuance** path, not at key resolution.

`DeviceAuthority::issue` signs with `self.root` (`authority.rs:268`), and the pairing path reaches it
through `authority.issue(&label, role, &request.public_key)` (`crates/arreo-cli/src/main.rs:2679`).
One of these must differ from the file's key at issue time, or the certificate's signed payload must
not be what `DeviceCert::verify` recomputes.

## Acceptance criteria

- [ ] The cause is identified with evidence, not inference: a test that loads the root key and the
      certificate from the reproduction directory and calls `DeviceCert::verify` — printing what it
      checked — or an equivalent instrumented run. The answer either way is recorded here.
- [ ] `arreo pair` → self-admission produces a certificate that (a) the issuing machine's own
      authority loads back (step 4 above reports the device, not "no devices paired yet") and (b) the
      relay accepts (step 5 registers the machine).
- [ ] A regression test covers the self-admission path end to end — it is the first-machine bootstrap,
      so it is on the critical path for every new account, not an edge case.
- [ ] T-0066's exit-criterion script passes, timed, once this and T-0066's documentation half land.
- [ ] If the mechanism turns out to be that the pairing path signs with a *session* or *server*
      identity rather than the account root, say so explicitly and decide whether that is intended —
      it would mean self-admission is unsupported by design, which changes T-0066's fix from "document
      it" to "build a bootstrap command".

## Notes

- **Why p1:** this blocks the Phase 2 exit criterion (`a stranger pairs a second machine in < 5 min`)
  at the first machine, and it is also the only path by which a new account gets its first member.
- The failure is *silent at the point of the mistake*: `pair` reports success, writes the
  certificate, and the problem surfaces later as a relay refusal that reads like a network fault.
  That shape is worth fixing even if the underlying cause turns out to be small.
- Diagnosis was not completed in the turn that filed this. What is established, and what is not, is
  written above so the next attempt starts from evidence rather than from the beginning.
- Related: the relay's handshake budget (3 per 10 s per address) masks this on loopback by turning
  the retry storm into `refused to accept a new connection` — T-0066 records that.

## Verification

```console
bash .loop/evidence/T-0066/exit-criterion.sh   # the reproduction, timed
cargo test -p arreo-core --lib identity::authority
cargo xtask e2e --slice mesh
```
