# ADR 0001: PTY layer — `portable-pty` 0.9

- Status: accepted (2026-09-10, T-0002)
- Context: Arreo needs real PTYs on Linux, macOS, and Windows (ROADMAP §3.11:
  all three OSes first-class from Phase 0). The PTY manager must spawn, read,
  write, resize, and reap children per pane with bounded memory.
- Decision: use `portable-pty` 0.9 (WezTerm's crate) behind our own `Pane`
  wrapper (`crates/arreo-core/src/pty.rs`), which adds the reader-pump thread,
  the bounded 512-line ring buffer, and `Send+Sync` interior mutability.
- Why this one:
  - It is the only mature Rust PTY crate with a production ConPTY backend —
    WezTerm runs it daily on Windows. Any Unix-only approach (`fork` +
    `posix_openpt` via `nix`/`libc`) would leave Windows as a second
    implementation to write and debug, violating the anti-"ported-later" rule.
  - Trait-based (`PtySystem` / `MasterPty` / `Child`) with runtime backend
    selection — exactly the seam T-0007 needs to exercise ConPTY on a Windows
    runner against the same `Pane` API.
  - Small, stable API surface; version 0.9 resolves offline in this environment.
- Alternatives rejected:
  - Hand-rolled `nix`/`libc` PTY: Unix-only; ConPTY FFI by hand is a security
    and correctness risk with no payoff (rejected: platform coverage).
  - `tokio-pty` / `pty-process` style async wrappers: thinner ecosystems, no
    ConPTY story as strong as WezTerm's (rejected: Windows risk).
  - Driving `tmux`/`screen` as subprocesses: external dependency, extra
    parsing layer, version skew (rejected: complexity + fragility).
- Consequences: `Pane` must bridge `Send`-but-not-`Sync` handles with mutexes
  (documented on the struct); reader pump is one OS thread per pane (30 panes
  = 30 mostly-blocked threads — acceptable; revisit with async I/O only if
  the T-0008 bench says so).
