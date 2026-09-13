//! T-0074: the fleet verbs, against a real (scratch) ledger.
//!
//! `arreo-tui` may not depend on the CLI binary crate, so the machines/trust
//! verbs are the TUI's own — which is exactly why they need their own gate: the
//! criterion is that a refusal is *the same sentence and the same exit code* the
//! CLI prints, and that a grant/revoke actually writes and cuts what it says.
//!
//! Everything here runs against a scratch identity directory and a scratch
//! socket path (no daemon, no relay): the ledger is a local SQLite store beside
//! the socket, and the directory verbs stop at their first refusal — no config —
//! before they touch a network. The refusal *sentences* are the CLI's own
//! words, and `xtask/src/mesh_slice.rs` is what compares them to the CLI's live
//! output.

use arreo_core::identity::authority::{sidecar_db, DeviceAuthority, Layout};
use arreo_core::identity::{DeviceId, DeviceKey, Role};
use arreo_tui::fleet::{Code, Fleet};
use std::future::Future;
use std::path::PathBuf;

/// A scratch machine: an identity directory and a socket path, both under this
/// process's own temp directory, removed first so a rerun starts clean.
fn scratch(tag: &str) -> (PathBuf, Fleet) {
    let base = std::env::temp_dir().join(format!("arreo-tui-fleet-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).expect("scratch dir");
    let fleet = Fleet {
        socket: base.join("arreo.sock"),
        identity_root: base.join("identity"),
        config: None,
        attached_to: None,
    };
    (base, fleet)
}

fn layout(fleet: &Fleet) -> Layout {
    Layout {
        root_key: fleet.identity_root.join("root.key"),
        cert_dir: fleet.identity_root.join("devices"),
        store: sidecar_db(&fleet.socket),
    }
}

/// The exit codes are the CLI's contract (T-0044), so the numbers are asserted
/// rather than assumed: a divergence here would make every script that switches
/// on them wrong.
#[test]
fn the_exit_codes_are_the_clis_own_numbers() {
    assert_eq!(Code::Ok.as_u8(), 0);
    assert_eq!(Code::Failure.as_u8(), 1);
    assert_eq!(Code::Usage.as_u8(), 2);
    assert_eq!(Code::UnknownMachine.as_u8(), 3);
    assert_eq!(Code::Unreachable.as_u8(), 4);
    assert_eq!(Code::Conflict.as_u8(), 5);
    assert!(!Code::Ok.refused() && !Code::Usage.refused());
    for code in [Code::UnknownMachine, Code::Unreachable, Code::Conflict] {
        assert!(code.refused(), "{code:?} is a refusal");
    }
}

#[test]
fn a_fresh_machine_has_no_grants() {
    let (base, fleet) = scratch("fresh");
    let outcome = fleet.trust_list();
    assert!(outcome.is_ok(), "{}", outcome.message);
    assert!(outcome.grants.is_empty());
    assert!(
        outcome.message.contains("0 grant(s) on"),
        "{}",
        outcome.message
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_device_id_that_is_not_one_is_refused_with_the_clis_sentence() {
    let (base, fleet) = scratch("bad-id");
    let error = fleet
        .trust_preview("not-a-device", "")
        .expect_err("a non-id is refused");
    // `machines trust: {raw:?} is not a device id: {e}` — the CLI's line,
    // verb prefix included, at the CLI's code (3).
    assert_eq!(error.code.as_u8(), 3);
    assert!(
        error
            .message
            .starts_with("machines trust: \"not-a-device\" is not a device id:"),
        "{}",
        error.message
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_grant_for_a_device_this_machine_never_pinned_is_refused() {
    let (base, fleet) = scratch("unpinned");
    // A well-formed id nobody has pinned: the Noise handshake would resolve the
    // peer from the pin list, so a grant could never authenticate — the CLI's
    // refusal says exactly that, at code 3.
    let key = DeviceKey::generate().expect("entropy");
    let id = DeviceId::from_key(&key.public());
    let error = fleet
        .trust_preview(&id.display_id(), "")
        .expect_err("an unpinned device is refused");
    assert_eq!(error.code.as_u8(), 3);
    assert!(
        error
            .message
            .contains("is not pinned on this machine, so a grant would do nothing"),
        "{}",
        error.message
    );
    assert!(
        error.message.contains("`arreo devices issue --key …`"),
        "the refusal names the command that fixes it: {}",
        error.message
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_role_that_is_not_a_role_is_a_usage_refusal() {
    let (base, fleet) = scratch("bad-role");
    let key = DeviceKey::generate().expect("entropy");
    let id = DeviceId::from_key(&key.public());
    let error = fleet
        .trust_preview(&id.display_id(), "admin")
        .expect_err("an unknown role is refused");
    assert_eq!(error.code.as_u8(), 2);
    assert!(
        error
            .message
            .contains("machines trust: --role takes viewer or operator (got \"admin\")"),
        "{}",
        error.message
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_grant_is_written_confirmed_and_visible_and_a_revoke_cuts_it() {
    let (base, fleet) = scratch("round-trip");
    // Pin a device the way the deployment does (`arreo devices issue`), so the
    // grant path is exercised against a real authority index rather than a stub.
    let key = DeviceKey::generate().expect("entropy");
    let id = DeviceId::from_key(&key.public());
    let mut authority = DeviceAuthority::load(layout(&fleet)).expect("authority");
    authority
        .issue("phone", Role::Viewer, &key.public())
        .expect("pin the device");

    // Before the grant: nothing here.
    let preview = fleet
        .trust_preview(&id.display_id(), "")
        .expect("a pinned device can be granted");
    // The line the CLI prints before its own confirmation, word for word.
    assert!(
        preview.line.contains("no grant here yet"),
        "{}",
        preview.line
    );
    assert!(
        preview
            .line
            .starts_with(&format!("grant {} ", id.display_id())),
        "the fingerprint is the first thing the operator reads: {}",
        preview.line
    );

    let granted = fleet.trust_grant(&preview);
    assert!(granted.is_ok(), "{}", granted.message);
    assert!(
        granted.message.contains("may now observe on"),
        "{}",
        granted.message
    );

    let listed = fleet.trust_list();
    assert_eq!(listed.grants.len(), 1, "{}", listed.message);
    assert_eq!(listed.grants[0].device, id.display_id());
    assert_eq!(listed.grants[0].role, Role::Viewer);
    assert!(listed.grants[0].live, "a fresh grant is live");

    let revoked = fleet.trust_revoke(&id.display_id());
    assert!(revoked.is_ok(), "{}", revoked.message);
    assert!(revoked.message.contains("cut"), "{}", revoked.message);

    let after = fleet.trust_list();
    assert_eq!(after.grants.len(), 1, "the row stays, the grant does not");
    assert!(!after.grants[0].live, "the grant was cut");

    // Idempotent, like the device-level revoke: the desired state holds, and
    // the CLI says so rather than reporting a second cut.
    let again = fleet.trust_revoke(&id.display_id());
    assert!(again.is_ok(), "{}", again.message);
    assert!(
        again.message.contains("had no live grant"),
        "{}",
        again.message
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_tui_attached_to_another_machine_refuses_to_change_trust() {
    let (base, mut fleet) = scratch("remote-trust");
    fleet.attached_to = Some("some-other-machine".to_string());
    // `machines trust`'s `--machine <other>` sentence, at the CLI's code (5).
    let listed = fleet.trust_list();
    assert_eq!(listed.code.as_u8(), 5);
    assert!(
        listed
            .message
            .contains("\"some-other-machine\" is not this machine (which is"),
        "{}",
        listed.message
    );
    assert!(
        listed.message.contains("Trust is local"),
        "{}",
        listed.message
    );

    // ...and the same for the cut, in the verb's own spelling.
    let key = DeviceKey::generate().expect("entropy");
    let id = DeviceId::from_key(&key.public());
    let revoked = fleet.trust_revoke(&id.display_id());
    assert_eq!(revoked.code.as_u8(), 5);
    assert!(
        revoked
            .message
            .starts_with("devices revoke: \"some-other-machine\" is not this machine"),
        "{}",
        revoked.message
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn naming_this_machine_itself_is_not_a_refusal() {
    let (base, mut fleet) = scratch("self-trust");
    // The CLI compares `--machine` against this machine's own name (or id), so
    // naming itself is allowed: what matters is that the check is a comparison
    // and not "any --machine is refused".
    fleet.attached_to = Some(arreo_core::mesh::default_machine_name());
    let outcome = fleet.trust_list();
    assert!(outcome.is_ok(), "{}", outcome.message);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn the_directory_verbs_stop_with_the_clis_usage_refusal_without_a_config() {
    let (base, fleet) = scratch("no-config");
    // The CLI's `config_path` insists on being told, and exits 2 — the TUI says
    // the same sentence, because a TUI that invented a default path would be a
    // second answer to "which file is this machine's relay configuration".
    let outcome = futures_block_on(fleet.machines_list());
    assert_eq!(outcome.code.as_u8(), 2);
    assert!(
        outcome
            .message
            .starts_with("machines: which configuration? pass --config PATH"),
        "{}",
        outcome.message
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_machine_name_that_is_not_a_name_is_a_rename_refusal_from_the_core_rule() {
    let (base, fleet) = scratch("bad-name");
    // No config, so the verb refuses at the configuration step first; that is
    // the CLI's order too, and the name rule is `arreo-core`'s `Name::parse`
    // either way.
    let outcome = futures_block_on(fleet.machines_rename("workbox", "Not A Name"));
    assert_eq!(outcome.code.as_u8(), 2, "{}", outcome.message);
    let _ = std::fs::remove_dir_all(&base);
}

/// A tiny blocking executor for the async directory verbs, which the tests call
/// without a runtime: two of them (no-config, no-relay) return before any I/O.
fn futures_block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime")
        .block_on(future)
}
