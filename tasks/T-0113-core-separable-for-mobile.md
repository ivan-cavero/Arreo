---
id: T-0113
title: Make `arreo-core` separable so the mobile artifact carries no store
phase: 3
priority: 3
status: proposed
depends_on: [T-0104]
scope:
  - crates/arreo-core/src/mesh/**
  - crates/arreo-core/src/store.rs
  - crates/arreo-core/Cargo.toml
  - crates/arreo-core-ffi/Cargo.toml
  - xtask/src/ffi.rs
  - docs/mobile.md
  - .loop/evidence/T-0113/**
verify:
  - cargo build -p arreo-core-ffi
  - cargo test --workspace
  - cargo xtask check-targets
---

## Why this exists

Found by the T-0104 review (finding 6), and **attempted** during that task's integration:
`crates/arreo-core-ffi/Cargo.toml` was changed to
`arreo-core = { default-features = false, features = ["transport"] }`, which is the right
intent — a phone is a client with no store — and it **does not compile**:

```
error[E0433]: cannot find `store` in `crate`
   --> crates/arreo-core/src/mesh/resolve.rs:112:20
    |
112 |             crate::store::rfc3339_ms(self.last_seen_ms)
```

`arreo-core`'s `default = ["sqlite", "transport"]`, and `mesh::resolve` calls
`crate::store::rfc3339_ms` unconditionally while `store` is gated behind `sqlite`. So the
"store is optional" claim is true of the *manifest* and false of the *code*.

The change was reverted rather than smuggled in: T-0104's scope fence is
`crates/arreo-core-ffi/**` + `docs/mobile.md` + `xtask/src/main.rs`, and editing
`arreo-core`'s module layout is outside it. This task is that change.

## Why it matters

`nm --defined-only target/debug/libarreo_core_ffi.so` shows **15 sqlite symbols** in the
shipped cdylib, and `cargo tree -p arreo-core-ffi -e normal` shows
`rusqlite → libsqlite3-sys`. Nothing this crate exports can reach them: a phone links a
database engine it cannot call, in a product whose ROADMAP §3.5 size discipline is
explicit ("the Rust .a adds ~2–6 MB", `opt-level="z"`, `strip`). The crate already prunes
uniffi's own features for exactly this reason, so the intent is established; only the
core's separability is missing.

PTY weight is the same class and arrives through `portable-pty`, which is **not** optional
in `arreo-core` (`nm` shows ~197 PTY symbols). Deciding whether a client-only core can drop
it is part of this task's design, not a given — the FFI crate may genuinely need none of
`pty`, but `arreo-core`'s modules may not be separable along that line either. Measure
before promising.

## Acceptance criteria

- [ ] **The reason `default-features = false` failed is fixed**: whatever `mesh::resolve`
      needs from `store` is either feature-independent (moved to a module that is always
      compiled) or itself gated. `cargo build -p arreo-core-ffi` with
      `default-features = false, features = ["transport"]` compiles.
- [ ] **The measurement is stated, not asserted**: `nm --defined-only` on the freshly built
      cdylib counts sqlite (and, if addressed, PTY) symbols **before and after**, both
      numbers in the evidence. A claim of "no store" without the symbol count is a claim,
      not a proof.
- [ ] **`check-targets` stays PASS/SKIP** — dropping `sqlite` is the feature that gate uses
      for the pure-Rust foreign-target build, so a mistake here breaks the Windows/macOS
      check in a way that looks unrelated.
- [ ] **The workspace still builds and tests green** with the default features on: this must
      not make the daemon or the CLI store-optional. Feature unification re-enables `sqlite`
      through `arreo-server` in a workspace build, which is expected — the criterion is that
      the *per-package* build is store-free and the workspace build is unchanged.
- [ ] `docs/mobile.md`'s weight paragraph updated with the measured numbers, and
      `crates/arreo-core-ffi/Cargo.toml`'s comment replaced (it currently records this as
      filed rather than fixed).

## Notes

- If PTY turns out to be inseparable without a large core refactor, that is a legitimate
  outcome: state the measured cost, keep the store fix, and file the PTY half separately.
  Do not refactor `arreo-core`'s module tree for a symbol count.
