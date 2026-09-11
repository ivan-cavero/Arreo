//! License-boundary gate (T-0035 acceptance criteria, extending T-0001).
//!
//! Enforcement mechanism: a plain `cargo test` (runs in CI on all three OSes).
//! Rules:
//!   1. `arreo-server` and `arreo-cli` depend on `arreo-core`.
//!   2. No other workspace crate depends on `arreo-server`.
//!   3. AGPL boundary (ROADMAP §7 + §9): `arreo-relay` is the only crate that
//!      may link AGPL code, and it is AGPL itself. `arreo-relay` may depend on
//!      Apache first-party crates (one-way, today only `arreo-core`); **no
//!      Apache first-party crate may depend on `arreo-relay`** — the relay is
//!      spoken to over the documented protocol or run as the unmodified binary.
//!   4. Declared licenses: every shipped crate is Apache-2.0, the relay is
//!      AGPL-3.0-or-later, and the workspace default stays Apache-2.0.
//!   5. `REUSE.toml` maps `crates/arreo-relay/**` to AGPL and names no path that
//!      does not exist (the protocol vocabulary is Apache and lives in
//!      `arreo-core`, T-0029 — that is what makes it implementable from other
//!      code).
//!   6. No convenience shortcut: `arreo-cli` has no `relay` verb (linking the
//!      relay into the CLI would relicense the CLI; an exec shim is a second
//!      name for one thing).
//!
//! `xtask` itself is excluded (dev tooling, never shipped): it is free to
//! depend on anything the harnesses need.
//!
//! Honest gap (T-0035 notes): this gate reads manifests and REUSE textually —
//! it catches dependency edges, not copy-pasted code.

use std::path::PathBuf;

fn ws_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the workspace root")
        .to_path_buf()
}

fn crate_manifest(name: &str) -> String {
    let path = ws_root().join("crates").join(name).join("Cargo.toml");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

fn depends_on(manifest: &str, dep: &str) -> bool {
    manifest.lines().any(|line| {
        let line = line.trim();
        !line.starts_with('#') && line.contains(dep)
    })
}

/// The `license` the manifest declares for its crate: an explicit
/// `license = "..."` wins; otherwise the crate inherits the workspace default.
fn declared_license(crate_name: &str, manifest: &str) -> String {
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some(value) = line.strip_prefix("license") {
            let value = value
                .trim()
                .trim_start_matches(['=', '.'])
                .trim_start_matches("workspace")
                .trim()
                .trim_start_matches('=')
                .trim();
            let value = value.trim_matches('"');
            if value == "true" {
                // `license.workspace = true`: inherits the workspace default.
                return workspace_license();
            }
            if value == "workspace" || value.ends_with(".workspace") {
                return workspace_license();
            }
            return value.to_string();
        }
    }
    panic!("{crate_name} declares no license field");
}

fn workspace_license() -> String {
    let path = ws_root().join("Cargo.toml");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some(value) = line.strip_prefix("license") {
            let value = value.trim().trim_start_matches('=').trim();
            return value.trim_matches('"').to_string();
        }
    }
    panic!("workspace declares no license");
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

/// T-0035: the relay sits atop the first-party graph. Apache code may enter an
/// AGPL work; AGPL code may not enter an Apache work (the binary would be
/// AGPL). So the relay may depend on `arreo-core`, and nothing Apache may
/// depend on the relay.
#[test]
fn nothing_apache_depends_on_the_relay() {
    for name in [
        "arreo-core",
        "arreo-server",
        "arreo-cli",
        "arreo-tui",
        "arreo-plugin-api",
    ] {
        let manifest = crate_manifest(name);
        assert!(
            !depends_on(&manifest, "arreo-relay"),
            "{name} must not depend on arreo-relay: Apache code may not link the AGPL relay"
        );
    }
}

/// T-0035: the relay's only first-party edge is the Apache core it speaks.
#[test]
fn the_relay_depends_only_on_apache_core() {
    let manifest = crate_manifest("arreo-relay");
    assert!(
        depends_on(&manifest, "arreo-core"),
        "arreo-relay must depend on arreo-core (the Apache protocol vocabulary)"
    );
    for name in ["arreo-server", "arreo-cli", "arreo-tui", "arreo-plugin-api"] {
        assert!(
            !depends_on(&manifest, name),
            "arreo-relay must not depend on {name}: the relay sits atop the graph, not in it"
        );
    }
}

/// T-0035: manifest truth. The workspace default stays Apache-2.0; every
/// shipped crate declares Apache (directly or by inheritance); the relay
/// declares AGPL-3.0-or-later explicitly — inheriting the workspace default
/// would silently license it Apache, which is the live bug this task fixes.
#[test]
fn declared_licenses_are_apache_except_the_relay() {
    assert_eq!(
        workspace_license(),
        "Apache-2.0",
        "the workspace default must stay Apache-2.0"
    );
    for name in [
        "arreo-core",
        "arreo-server",
        "arreo-cli",
        "arreo-tui",
        "arreo-plugin-api",
    ] {
        let manifest = crate_manifest(name);
        assert_eq!(
            declared_license(name, &manifest),
            "Apache-2.0",
            "{name} must declare Apache-2.0"
        );
    }
    let relay = crate_manifest("arreo-relay");
    assert_eq!(
        declared_license("arreo-relay", &relay),
        "AGPL-3.0-or-later",
        "arreo-relay must declare AGPL-3.0-or-later explicitly, not inherit the workspace default"
    );
    assert!(
        !relay.lines().any(|line| {
            let line = line.trim();
            !line.starts_with('#') && line.starts_with("license.workspace")
        }),
        "arreo-relay must not use license.workspace (that is Apache-2.0)"
    );
}

/// T-0035: `REUSE.toml` maps the relay to AGPL and names no path that does not
/// exist. The stale `crates/arreo-relay-proto/**` entry is retired: the
/// protocol vocabulary is Apache and lives in `arreo-core` (T-0029), which is
/// what makes it implementable from other code.
#[test]
fn reuse_maps_the_relay_to_agpl_and_names_real_paths() {
    let path = ws_root().join("REUSE.toml");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    assert!(
        text.contains("crates/arreo-relay/**"),
        "REUSE.toml must map crates/arreo-relay/**"
    );
    assert!(
        !text.contains("arreo-relay-proto"),
        "REUSE.toml must not name crates/arreo-relay-proto/**: that path does not exist"
    );
    // Every globbed path in the file must exist on disk — a mapping for a path
    // that is not there is a claim about nothing.
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || !line.starts_with("path") {
            continue;
        }
        let Some(list) = line.split('=').nth(1) else {
            continue;
        };
        for entry in list.split(',') {
            let entry = entry.trim().trim_matches(['"', '[', ']', ' ']);
            if entry.is_empty() {
                continue;
            }
            let base = entry.trim_end_matches("/**").trim_end_matches("/*");
            assert!(
                ws_root().join(base).exists(),
                "REUSE.toml names {entry}, which does not exist"
            );
        }
    }
}

/// T-0035: the protocol is implementable without AGPL code. The envelope types
/// the relay speaks must be reachable from the Apache `arreo-core` public API —
/// the frames T-0023 carries — so a third party implements from the protocol
/// document, not from the relay crate.
#[test]
fn the_protocol_vocabulary_is_reachable_from_apache_core() {
    let lib = ws_root().join("crates/arreo-core/src/relay/mod.rs");
    let text = std::fs::read_to_string(&lib)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", lib.display()));
    for item in [
        "pub enum RelayKind",
        "pub struct RelayHeader",
        "pub struct RelayEnvelope",
        "pub enum Outcome",
        "pub struct DrainRequest",
        "pub struct DrainReport",
        "pub struct Ack",
    ] {
        assert!(
            text.contains(item),
            "arreo-core must export {item} (the Apache-side protocol vocabulary)"
        );
    }
    let client = ws_root().join("crates/arreo-core/src/relay/client.rs");
    let text = std::fs::read_to_string(&client)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", client.display()));
    assert!(
        text.contains("pub struct RelayClient"),
        "arreo-core must export the reference RelayClient"
    );
}

/// T-0035: no convenience shortcut. `arreo relay serve` stays unimplemented —
/// linking the relay into `arreo-cli` would relicense the CLI, and an exec shim
/// is a second name for one thing. The CLI ships no verb that starts the relay.
#[test]
fn the_cli_has_no_relay_verb() {
    let main = ws_root().join("crates/arreo-cli/src/main.rs");
    let text = std::fs::read_to_string(&main)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", main.display()));
    for line in text.lines() {
        let code = line.split("//").next().unwrap_or("");
        // A match arm dispatching on a "relay" subcommand — not a comment, not
        // a socket path, not the pairing mailbox.
        let code = code.trim();
        if code.starts_with("Some(\"relay\")") || code.starts_with("\"relay\" =>") {
            panic!("arreo-cli must not grow a `relay` verb: {line}");
        }
    }
}
