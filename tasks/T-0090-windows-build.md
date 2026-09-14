---
id: T-0090
title: Windows builds again — cfg-gate the Unix-only daemon/handoff, honest deferred story
phase: 2
priority: 1
status: proposed
depends_on: [T-0038, T-0039]
scope:
  - crates/arreo-server/src/daemon.rs
  - crates/arreo-server/src/handoff.rs
  - crates/arreo-core/src/pty.rs
  - crates/arreo-core/src/pty/**
  - crates/arreo-core/src/update/mod.rs
  - crates/arreo-core/src/enforce/other.rs
  - xtask/src/check_targets.rs
  - xtask/src/main.rs
  - .loop/evidence/T-0090/**
verify:
  - cargo test --workspace
  - cargo clippy --workspace --all-targets -- -D warnings
  - cargo fmt --all -- --check
---

## Goal

`arreo-server` does not compile on Windows (35 errors on `windows-latest`): `std::os::unix::*`
in `daemon.rs`/`handoff.rs` (PermissionsExt, AsFd/BorrowedFd/OwnedFd, UnixStream/UnixListener,
`from_mode`, `master_fd`, `ExclusiveLock::fd`), ungated `tokio::net::UnixListener/UnixStream`
imports, and uses of the `#[cfg(unix)]`-gated `pty::adopt` module. The T-0038 sprint wrote
Unix-only code and the local gate did not catch it. Multiplatform is a hard requirement
(ROADMAP §3.11), so this is p1.

## Acceptance criteria

- [ ] `cargo build --workspace --all-targets` green on the `windows-latest` runner.
- [ ] Every Unix-only item cfg-gated with a Windows answer, never a hole: live handoff
      refuses honestly through the T-0039 deferred path ("applies at next restart",
      never a panic or a silent no-op); no `todo!`/`unimplemented!` left on the Windows
      build.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean on Windows: the
      Windows-dead items (`OtherProbe`, `WakeReader`/`WakeWriter`, `take_reader`,
      `wake_channel`, `TapIo`, unused `path`) gated or removed with a one-line reason
      each — no blanket `allow(dead_code)`.
- [ ] Gate hole closed: `check-targets` (T-0010) catches a reintroduced ungated `unix`
      import (mutation proof in evidence), or the task records why cross-compile cannot
      see this class and what replaces it. A second silent Windows rot is the failure.
- [ ] No Linux behavior change: full workspace battery + handoff slice green.

## Verification

```console
# On the runner: windows-latest build + clippy legs green.
cargo test --workspace   # local Linux half
```
