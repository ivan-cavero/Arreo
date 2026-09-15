---
id: T-0104
title: UniFFI bindings — one core, three UIs, and the mobile half starts here
phase: 3
priority: 2
status: done
depends_on: [T-0025, T-0029, T-0044]
scope:
  - crates/arreo-core-ffi/**
  - docs/mobile.md
  - xtask/src/main.rs
  - .loop/evidence/T-0104/**
verify:
  - cargo test --workspace
  - cargo xtask ffi --check
  - cargo vet --locked
  - cargo deny check
evidence:
  - .loop/evidence/T-0104/ffi-bindings.txt
---

## Design (decided by the planner — the contract)

### The dependency: `uniffi` 0.32, and why not the alternatives

The task's Notes demand the rationale, so it is recorded here before a line is written.

| Option | Why it loses |
|---|---|
| Hand-written C ABI + `extern "C"` | No type or error mapping: every function is a manual edit in the Rust, the header, the Swift and the Kotlin, and a struct change silently desynchronises three files. Strings/`Result`/`Option`/enums all need hand-rolled marshalling. |
| `cbindgen` | Generates C headers only — the Swift and Kotlin wrappers are still hand-written, and there is no `async` story, which the relay session needs. |
| `flutter_rust_bridge` | Targets Flutter/Dart. ROADMAP §3.5 explicitly rejects a Flutter runtime ("No WebView, no Flutter runtime"); adding it to generate bindings for a stack we do not ship is scaffolding. |
| **UniFFI** | One definition generates **Swift and Kotlin**, with typed errors, records, enums and `async` — and it is the decision ROADMAP §3.5 already records (line 129: "exposed via **UniFFI**"). Chosen. |

`uniffi` is a **new dependency** (ledger note, as required). It is confined to the new crate
`arreo-core-ffi`; nothing that ships today links it, so the daemon/CLI/TUI binary sizes and
the existing supply-chain posture are unaffected except for the vet exemptions the new
crate's tree needs.

### The crate: `crates/arreo-core-ffi` — a leaf, and a client

- A new workspace member (the `members` list is explicit — add it).
- Depends on `arreo-core` and nothing else first-party. It is a **leaf**: nothing may depend
  on it, and it must satisfy `xtask/tests/workspace_deps.rs` (Apache-2.0 declared, no edge to
  `arreo-server` or `arreo-relay`) by *reading the gate*, not by working around it.
- `crate-type = ["cdylib", "staticlib", "lib"]`: the cdylib is what `uniffi-bindgen
  --library` reads, the staticlib is what an iOS app links.
- **No PTY, no daemon, no store.** A phone is a client: pairing (both sides), the device
  identity and its fingerprint, the relay session (connect/drain/ack), the machine directory
  list, the protocol codec, and the theme engine's tokens. If a function needs a filesystem
  or a child process, it does not belong here.

### Generating bindings

`uniffi-bindgen` ships as a binary in the ffi crate itself (`src/bin/uniffi-bindgen.rs`
calling `uniffi::uniffi_bindgen_main()`, behind a `cli` feature) — the standard shape, no
extra toolchain, and it is version-locked to the library it generates for. `xtask ffi`
invokes `cargo run -p arreo-core-ffi --features cli --bin uniffi-bindgen -- generate
--library <cdylib> --language {swift,kotlin} --out-dir <scratch>`.

### What `cargo xtask ffi --check` does — and the honest SKIPs

Probed on this box before writing this: **JDK 25 is installed; `kotlinc`, `swift` and
`swiftc` are not.** The criterion's parenthetical ("Kotlin bindings can be compiled with a
JDK if one is installed") is **wrong as written** — Kotlin needs the Kotlin compiler, not a
JDK — so the check is the `check-targets` pattern: probe, and report the SKIP with the
precise reason.

1. Build the ffi crate's cdylib (a real build; a failure is a FAIL, not a skip).
2. Generate **both** languages into scratch. A generator failure is a FAIL.
3. **Assert the generated surface**: the Swift and Kotlin outputs each contain the expected
   top-level names (the client surface list above) — a golden-symbol check, so "it generated
   an empty file" cannot pass.
4. **Compile the Kotlin** if `kotlinc` is on PATH; otherwise SKIP naming what is missing.
   If it is present, the classpath needs the JNA jar and the Kotlin stdlib — a probe that
   cannot find them is a SKIP that names them, never a fabricated PASS.
5. **Swift**: SKIP with the reason (needs macOS + Xcode; the sanctioned place to *execute*
   macOS is the GitHub runner, the same class of gate as T-0090).
6. The **contract test** is the strong proof and needs no toolchain: a Rust test that drives
   pairing and a session **through the exported FFI surface** (`#[uniffi::export]` items as
   a foreign caller sees them), not through the `arreo-core` API underneath it — so a
   boundary that cannot be crossed is caught here rather than in an Xcode build.

### The error rule

Errors cross as **typed** errors (`#[derive(uniffi::Error)]`), each carrying the same
sentence the CLI prints for that state — the T-0074 rule ("no silent divergences") applied to
the FFI boundary. A `Result<_, String>` anywhere in the exported surface is a finding.

## Goal

Phase 3 (ROADMAP §3.5) has **no tasks at all** — 0 of 60 filed — and its first bullet is "Rust
core via UniFFI; iOS and Android built in parallel — shared core means duplicated UI only". The
mobile UIs need Xcode and the Android SDK, neither of which exists on this box; the *bindings*
do not.

One sentence: the client-relevant core (pairing, device identity, the relay session, the
directory, the protocol codec, the theme engine) gets a typed UniFFI surface, so the two mobile
UIs are written against a generated API rather than against a Rust ABI.

## Acceptance criteria

- [x] `crates/arreo-core-ffi` exposes the client surface with UniFFI: pairing (both sides), the
      device identity and its fingerprint, relay session connect/drain/ack, `machines` list, the
      message codec, and the theme engine's tokens. No PTY, no filesystem, no daemon — a phone is
      a client.
- [x] Errors cross the boundary as **typed** errors with the same sentences the CLI prints (an
      `Error` enum, not a stringly-typed panic) — the "no silent divergences" rule T-0074
      applied to the TUI, applied here.
- [x] `cargo xtask ffi --check` generates bindings for both target languages and **compiles what
      it generated** where a toolchain exists, reporting an honest SKIP where it does not (the
      `check-targets` pattern). **Corrected while planning:** a JDK alone cannot compile Kotlin
      (it needs `kotlinc`, which this box does not have) — so Kotlin is a probe-and-report, and
      Swift is SKIPped with the reason (needs macOS + Xcode). Both SKIPs name exactly what is
      missing; neither is a fabricated PASS.
- [x] The generated surface has a contract test: a Rust-side smoke test drives pairing + a
      session through the FFI layer itself (not through the Rust API underneath it), so a
      boundary that cannot actually be crossed is caught here rather than in an iOS build.
- [x] `docs/mobile.md` records the API surface, the two SKIP reasons, and what a mobile UI still
      has to bring (store, push, QR camera) — the honest split between this box and the
      toolchains it does not have.

## Notes

- This is deliberately the *only* Phase 3 task that can be finished here, and it is the one
  everything else in Phase 3 depends on. The iOS/Android UI tasks are drafted when this lands,
  and their proof is a store build — a different machine, the same class of gate as T-0090.
- UniFFI is a new dependency: ledger note with the rationale, and the alternatives considered
  (a hand-written C ABI, `cbindgen`, `flutter_rust_bridge`) recorded with why UniFFI wins for
  Swift + Kotlin from one definition.

## Outcome

Done. `crates/arreo-core-ffi` (a leaf, cdylib+staticlib+lib) exports the client surface —
85 symbols across pairing both sides, identity and fingerprint, the relay session, the
machine directory, the codec and the theme tokens — with 8 flat `#[derive(uniffi::Error)]`
enums and no `Result<_, String>` anywhere. `cargo xtask ffi --check` builds, generates both
languages, asserts the generated surface against a 85-symbol golden list in both directions,
and compiles the Kotlin where `kotlinc` exists (it does not here) while SKIPping Swift with
its reason. 10 contract tests drive the exported items, not the core beneath them.

**The task's premise was corrected before delegating:** "Kotlin bindings can be compiled with
a JDK" is false — a JDK cannot compile Kotlin. The criterion is now a probe-and-report.

**An independent review returned ship-with-follow-ups** and verified the three things worth a
second pair of eyes: the surface is client-shaped, the golden assertion cannot pass vacuously
(it checks generated output, both directions), and the Kotlin compile branch is a real
`kotlinc` run rather than a stub. Nine of its ten findings are fixed — including a security
one: **`device_cert_issue` bypassed the core's weak-key refusal**, so a malicious phone could
push a small-order ed25519 point (a key whose signatures can be forged for almost any
message) through the only issuing door a mobile UI has, pinning an identity the CLI refuses.
Now refused with the core's own predicate and its own sentence, with a regression test and a
control.

**One finding is filed, not fixed.** The shipped library still carries SQLite (53 symbols)
and PTY (197) that no export can reach. The obvious fix —
`arreo-core = { default-features = false, features = ["transport"] }` — **does not compile**,
because `arreo-core::mesh::resolve` calls `store::rfc3339_ms` while `store` is gated behind
`sqlite`. That is a change to `crates/arreo-core/src/**`, outside this task's fence, so it was
reverted with the reason in the manifest and filed as **T-0113**.
