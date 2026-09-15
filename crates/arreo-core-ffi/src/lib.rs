//! `arreo-core-ffi` — the client-relevant core, over UniFFI (T-0104).
//!
//! One sentence: the phone-shaped half of `arreo-core` — pairing, device
//! identity, the relay session, the machine directory, the protocol codec and
//! the theme engine's tokens — gets one typed definition from which UniFFI
//! generates the Swift and the Kotlin, so the two mobile UIs are written against
//! a generated API rather than against a Rust ABI.
//!
//! ## What this crate is, and what it deliberately is not
//!
//! It is a **leaf**: nothing depends on it, and it depends on `arreo-core` and
//! nothing else first-party (enforced by `xtask/tests/workspace_deps.rs`). It is
//! a **client**: a phone is not a server, so there is no PTY, no daemon, no
//! store, and no function here takes a filesystem path or spawns a child. If a
//! call would need a filesystem it does not belong here — the identity surface
//! takes a **seed** from the caller for exactly that reason (the platform's
//! keystore is the store; this crate is a pure function of the bytes it is
//! handed).
//!
//! ## The error rule (the contract)
//!
//! Every fallible call returns a typed `#[derive(uniffi::Error)]` enum whose
//! message is the sentence the core prints — which is the sentence every CLI
//! verb prefixes with its own name, so the phone and the CLI cannot say
//! different things about the same state (T-0074's "no silent divergences").
//! A `Result<_, String>` in this surface would be a finding; there is none.
//!
//! The enums are **flat** errors (`#[uniffi(flat_error)]`) on purpose, and the
//! reason is measurable rather than stylistic: UniFFI renders a *rich* error
//! variant's foreign `message` from its **fields** (`"field=${field}"`, see
//! `ErrorTemplate.kt`), so a rich variant would replace the core's sentence with
//! `got=2, want=4` — the sentence would be gone from the only place a UI reads
//! it. A flat error crosses as a variant tag plus the Rust `Display`, which is
//! exactly "typed, and carrying the sentence". The structured detail stays on
//! the Rust side of the boundary.
//!
//! ## Where the Rust-side contract test lives
//!
//! `tests/contract.rs` drives pairing and a session through the items below —
//! the `#[uniffi::export]` surface as a foreign caller sees it — against a
//! test-local mailbox and relay that speak the shipped wire protocol. It never
//! calls the `arreo-core` API underneath, so a boundary that cannot be crossed
//! fails here rather than in an Xcode build.

pub mod codec;
pub mod directory;
pub mod errors;
pub mod identity;
pub mod pairing;
pub mod relay;
pub mod theme;

uniffi::setup_scaffolding!();
