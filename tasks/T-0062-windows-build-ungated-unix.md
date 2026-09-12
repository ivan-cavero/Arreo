---
id: T-0062
title: The Windows build — the relay's pairing mailbox uses unix sockets ungated
phase: 2
priority: 2
status: done
depends_on: []
scope:
  - crates/arreo-relay/src/pairing.rs
  - crates/arreo-relay/src/main.rs
  - .loop/evidence/T-0062/**
---

## Goal

`cargo build --workspace --all-targets` fails on `windows-latest`. It has failed on every
CI run since the repository was created (90 of 90 runs are red — see T-0063), and this is the
cause of the Windows leg: `crates/arreo-relay/src/pairing.rs` imports
`std::os::unix::net::{UnixListener, UnixStream}` at module scope, and `pub mod pairing;` in
`lib.rs` carries no `#[cfg(unix)]` — so the crate does not compile for
`x86_64-pc-windows-msvc` at all.

The asymmetry is the tell: `arreo-core/src/pairing/wire.rs` — the same protocol, the client
half — gates its unix imports (`#[cfg(unix)]` at lines 33 and 35, and at each test that needs
a socket). The relay's copy of the same idea was written without the gate.

## Acceptance criteria

- [x] `cargo check --workspace --all-targets --target x86_64-pc-windows-msvc` reports **no
      rustc error in our code**: without the fix, the crate fails with `E0433: cannot find `unix`
      in `os``; with it, only the two C build-script failures remain (ring, libsqlite3-sys:
      `lib.exe` — the SDK marker `check-targets` classifies as SKIP, and the MSVC runner has
      the SDK). Evidence `.loop/evidence/T-0062/`.
- [x] The pairing mailbox over **TCP still works on every platform**: `serve_tcp` is ungated
      and untouched, and the relay's own test suite (77 rows, incl. the TCP mailbox tests) is
      green.
- [x] On Windows, `--pairing-socket` is refused with a sentence naming the reason and the
      working alternative (`--pairing-tcp`), via `spawn_unix_mailbox`: exit 2, never silently
      ignored, never a panic. Compiled but not executed on Windows (no runner here), which is
      stated rather than claimed.
- [x] The gate is the cross-target check itself (it fails on the ungated import, passes with
      the gate), plus `#[cfg(unix)]` on the two entry points and the socket-path test.
- [x] `check-targets` passes (1 pass, 1 skip — the skip is the C-dep SDK, as designed), and
      the relay's suite is green.

## Notes

- **Filed rather than fixed under T-0048**, whose fence is docs/CI/xtask: a product compile
  error is a different deliverable with a different proof (a cross-target check, not a
  document audit). T-0048's gate is what surfaced it, which is the point of having a gate.
- The fix is small and two-sided: `#[cfg(unix)]` on the unix entry points in `pairing.rs`
  (and on the tests that use them), and a platform-honest branch where `main.rs` plumbs
  `--pairing-socket`.
- Nothing about the mailbox's security rules changes: the gate moves *where* the socket API
  is available, not what a session is allowed to do.

## Verification

```console
cargo check -p arreo-relay --target x86_64-pc-windows-msvc
cargo test -p arreo-relay
cargo xtask e2e --slice relay
cargo xtask check-targets
```

## Outcome

Done. The fix is what the notes said it would be: `#[cfg(unix)]` on the two entry points,
the import, and the socket-path test; a `spawn_unix_mailbox` helper that refuses
`--pairing-socket` on Windows with the working alternative. No mailbox rule changed.
