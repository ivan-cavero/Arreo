//! Dependency-direction gate (T-0001 acceptance criterion).
//!
//! Enforcement mechanism: a plain `cargo test` (runs in CI on all three OSes).
//! Rules:
//!   1. `arreo-server` and `arreo-cli` depend on `arreo-core`.
//!   2. No other workspace crate depends on `arreo-server`.
//!
//! `xtask` itself is excluded (dev tooling, never shipped): it is free to
//! depend on anything the harnesses need.

use std::path::PathBuf;

fn crate_manifest(name: &str) -> String {
    let ws_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the workspace root")
        .to_path_buf();
    let path = ws_root.join("crates").join(name).join("Cargo.toml");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

fn depends_on(manifest: &str, dep: &str) -> bool {
    manifest.lines().any(|line| {
        let line = line.trim();
        !line.starts_with('#') && line.contains(dep)
    })
}

#[test]
fn server_and_cli_depend_on_core() {
    for name in ["arreo-server", "arreo-cli"] {
        let manifest = crate_manifest(name);
        assert!(
            depends_on(&manifest, "arreo-core"),
            "{name} must depend on arreo-core"
        );
    }
}

#[test]
fn nothing_depends_on_server() {
    for name in ["arreo-core", "arreo-cli", "arreo-relay", "arreo-plugin-api"] {
        let manifest = crate_manifest(name);
        assert!(
            !depends_on(&manifest, "arreo-server"),
            "{name} must not depend on arreo-server"
        );
    }
}
