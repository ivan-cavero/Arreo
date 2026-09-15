//! `uniffi-bindgen`, built from this crate so it is version-locked to the
//! library it generates for (T-0104).
//!
//! The standard shape, and the reason it is a binary *here* rather than a
//! `cargo install`ed tool: the generator and the metadata it reads ship from one
//! `Cargo.lock`, so a cdylib and the bindings generated from it cannot be a
//! version apart. It is behind the `cli` feature so the shipped cdylib and
//! staticlib never carry a TOML parser, a template engine and `cargo metadata`.
//!
//! `xtask ffi --check` invokes it as:
//!
//! ```text
//! cargo run -p arreo-core-ffi --features cli --bin uniffi-bindgen -- \
//!     generate --library <cdylib> --language swift|kotlin --out-dir <scratch>
//! ```

fn main() {
    uniffi::uniffi_bindgen_main();
}
