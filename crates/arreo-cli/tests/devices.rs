//! T-0025 device lifecycle, driven through the real `arreo` binary.
//!
//! These are process-level tests on purpose: the acceptance criteria talk about
//! "a real second process holding a fresh keypair" and "the old key's next
//! connection fails", so the referee is the actual CLI, not a function call.
//! Each test uses its own identity dir and socket path, so they run in parallel
//! without sharing state.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The CLI binary under test (cargo builds it before running these).
fn arreo() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_arreo"))
}

struct Scratch {
    dir: PathBuf,
    socket: PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "arreo-cli-devices-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Self {
            socket: dir.join("arreo.sock"),
            dir,
        }
    }

    fn identity(&self) -> PathBuf {
        self.dir.clone()
    }

    /// Run the CLI with this scratch's identity dir and socket.
    fn run(&self, args: &[&str]) -> (bool, String) {
        self.run_with_identity(&self.identity(), args)
    }

    /// Run with a different `ARREO_IDENTITY_DIR`.
    ///
    /// The authority (root key + certificates) lives in the *server's* identity
    /// dir; a client's own key lives wherever that client keeps it. Tests use
    /// this to stand up a second "machine", and then present that machine's
    /// public key explicitly — exactly what the transport does with the key a
    /// peer proves possession of.
    fn run_with_identity(&self, identity: &Path, args: &[&str]) -> (bool, String) {
        let output = Command::new(arreo())
            .args(args)
            .arg("--socket")
            .arg(&self.socket)
            .env("ARREO_IDENTITY_DIR", identity)
            .output()
            .expect("arreo runs");
        let mut text = String::from_utf8_lossy(&output.stdout).to_string();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        (output.status.success(), text)
    }

    /// A second identity dir (a different "machine").
    fn other_identity(&self, tag: &str) -> PathBuf {
        self.dir.join(tag)
    }

    fn public_key_of(&self, identity: &Path) -> String {
        let (ok, out) = self.run_with_identity(identity, &["devices", "id", "--json"]);
        assert!(ok, "devices id failed: {out}");
        let value: serde_json::Value = serde_json::from_str(out.trim()).expect("json output");
        value["public_key"]
            .as_str()
            .expect("public_key")
            .to_string()
    }

    fn device_id_of(&self, identity: &Path) -> String {
        let (ok, out) = self.run_with_identity(identity, &["devices", "id", "--json"]);
        assert!(ok, "devices id failed: {out}");
        let value: serde_json::Value = serde_json::from_str(out.trim()).expect("json output");
        value["device"].as_str().expect("device").to_string()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn a_device_with_a_certificate_is_authorized_and_one_without_is_not() {
    let scratch = Scratch::new("authorize");
    // Identity A plays the client; B is a stranger with its own keypair.
    let client = scratch.other_identity("client");
    let stranger = scratch.other_identity("stranger");
    let client_key = scratch.public_key_of(&client);
    let stranger_key = scratch.public_key_of(&stranger);

    // Before any certificate exists, the key is refused (and the refusal names
    // the device). `authorize` with no key uses this machine's own client key,
    // which is the server-host case: the operator's machine is a device too.
    let (ok, out) = scratch.run(&["devices", "authorize"]);
    assert!(!ok, "an unpaired device must be refused: {out}");
    assert!(out.contains("has no certificate"), "{out}");

    // Issue for the client only.
    let (ok, out) = scratch.run(&[
        "devices",
        "issue",
        "--name",
        "pixel-7",
        "--role",
        "viewer",
        "--key",
        &client_key,
    ]);
    assert!(ok, "issue failed: {out}");
    assert!(out.contains("viewer"), "{out}");

    let (ok, out) = scratch.run(&["devices", "authorize", &client_key, "--json"]);
    assert!(ok, "the paired device must be allowed: {out}");
    let value: serde_json::Value = serde_json::from_str(out.trim()).expect("json");
    assert_eq!(value["allowed"], serde_json::json!(true));
    assert_eq!(value["role"], serde_json::json!("viewer"));

    // The stranger is still refused — a certificate for one key is not a
    // certificate for another.
    let (ok, out) = scratch.run(&["devices", "authorize", &stranger_key]);
    assert!(!ok, "an unrelated key must be refused: {out}");
    assert!(
        out.contains(&stranger_key[..8]) || out.contains("has no certificate"),
        "{out}"
    );

    // And the refusal is an auditable event, not a silent no.
    let (ok, out) = scratch.run(&["audit", "--limit", "20"]);
    assert!(ok, "audit failed: {out}");
    assert!(
        out.contains("has no certificate"),
        "refusal missing from audit: {out}"
    );
    assert!(out.contains("issued"), "issuance missing from audit: {out}");
}

#[test]
fn rotation_makes_the_old_key_stop_working_for_good() {
    let scratch = Scratch::new("rotation");
    let old_identity = scratch.other_identity("old");
    let new_identity = scratch.other_identity("new");
    let old_key = scratch.public_key_of(&old_identity);
    let new_key = scratch.public_key_of(&new_identity);

    let (ok, out) = scratch.run(&[
        "devices", "issue", "--name", "laptop", "--role", "owner", "--key", &old_key,
    ]);
    assert!(ok, "issue failed: {out}");
    let old_id = scratch.device_id_of(&old_identity);
    assert_eq!(scratch.device_id_of(&old_identity), old_id);

    let (ok, out) = scratch.run(&["devices", "rotate", "--device", &old_id, "--key", &new_key]);
    assert!(ok, "rotate failed: {out}");
    let new_id = scratch.device_id_of(&new_identity);
    assert!(
        out.contains(&new_id),
        "rotation did not name the new device: {out}"
    );

    // The new key works, the old key is refused with a reason that says where
    // the device went (a different process each time: durability included).
    let (ok, out) = scratch.run(&["devices", "authorize", &new_key]);
    assert!(ok, "the rotated key must be allowed: {out}");
    let (ok, out) = scratch.run(&["devices", "authorize", &old_key]);
    assert!(!ok, "the retired key must be refused: {out}");
    assert!(out.contains("rotated away"), "{out}");
    assert!(
        out.contains(&new_id),
        "the refusal must name the replacement: {out}"
    );
}

#[test]
fn revocation_is_durable_and_immediate() {
    let scratch = Scratch::new("revocation");
    let identity = scratch.other_identity("client");
    let key = scratch.public_key_of(&identity);
    let (ok, out) = scratch.run(&[
        "devices", "issue", "--name", "phone", "--role", "owner", "--key", &key,
    ]);
    assert!(ok, "issue failed: {out}");
    let device = scratch.device_id_of(&identity);
    assert!(scratch.run(&["devices", "authorize", &key]).0);

    let (ok, out) = scratch.run(&["devices", "revoke", &device]);
    assert!(ok, "revoke failed: {out}");

    // Immediately refused...
    let (ok, out) = scratch.run(&["devices", "authorize", &key]);
    assert!(!ok, "a revoked device must be refused: {out}");
    assert!(out.contains("revoked"), "{out}");
    // ...and from a fresh process that reads the store from scratch (that is
    // the durability claim: the decision is on disk, not in memory).
    let (ok, out) = scratch.run(&["devices", "authorize", &key]);
    assert!(!ok, "revocation must survive a restart: {out}");

    // Re-issuing for a revoked device is refused too: revocation is not a
    // speed bump you can step over by issuing again.
    let (ok, out) = scratch.run(&[
        "devices", "issue", "--name", "phone", "--role", "owner", "--key", &key,
    ]);
    assert!(!ok, "re-issuing for a revoked device must fail: {out}");
    assert!(out.contains("revoked"), "{out}");

    // The list shows the revocation rather than hiding it.
    let (ok, out) = scratch.run(&["devices", "list", "--json"]);
    assert!(ok, "{out}");
    let value: serde_json::Value = serde_json::from_str(out.trim()).expect("json");
    assert_eq!(value["devices"][0]["revoked"], serde_json::json!(true));
}

#[test]
fn the_listing_is_scriptable_and_the_store_keeps_no_secrets() {
    let scratch = Scratch::new("listing");
    let identity = scratch.other_identity("client");
    let key = scratch.public_key_of(&identity);
    scratch.run(&[
        "devices", "issue", "--name", "pixel", "--role", "viewer", "--key", &key,
    ]);

    let (ok, out) = scratch.run(&["devices", "list", "--json"]);
    assert!(ok, "{out}");
    let value: serde_json::Value = serde_json::from_str(out.trim()).expect("json");
    assert_eq!(value["devices"][0]["name"], serde_json::json!("pixel"));
    assert_eq!(value["devices"][0]["role"], serde_json::json!("viewer"));
    assert_eq!(value["devices"][0]["serial"], serde_json::json!(1));
    assert_eq!(value["devices"][0]["public_key"], serde_json::json!(key));
    assert!(value["root"].as_str().is_some_and(|root| root.len() == 64));

    // The client's private key never reaches the server's store: compare the
    // raw database bytes against the secrets that live in the identity files.
    let client_secret = std::fs::read_to_string(
        scratch
            .other_identity("client")
            .join("identity")
            .join("device.key"),
    )
    .expect("client key file")
    .trim()
    .to_string();
    let root_secret = std::fs::read_to_string(scratch.identity().join("identity").join("root.key"))
        .expect("root key file")
        .trim()
        .to_string();
    let mut db = scratch.socket.clone().into_os_string();
    db.push(".db");
    let db_bytes = std::fs::read(PathBuf::from(db)).expect("store exists after issuing");
    let db_text = String::from_utf8_lossy(&db_bytes);
    assert!(
        !db_text.contains(&client_secret),
        "the device secret reached the store"
    );
    assert!(
        !db_text.contains(&root_secret),
        "the root secret reached the store"
    );
    // The public halves are in there, which is the point (they are not secret).
    assert!(
        db_text.contains(&key[..16]),
        "the public key should be stored"
    );
}

#[test]
fn a_viewer_may_observe_but_never_drive() {
    let scratch = Scratch::new("roles");
    let identity = scratch.other_identity("phone");
    let key = scratch.public_key_of(&identity);
    scratch.run(&[
        "devices", "issue", "--name", "phone", "--role", "viewer", "--key", &key,
    ]);

    // Observe: allowed, and the answer says which role made the decision.
    for verb in ["read", "attach", "wait", "metrics", "panes"] {
        let (ok, out) = scratch.run(&["devices", "authorize", &key, "--verb", verb]);
        assert!(ok, "a viewer must be allowed to {verb}: {out}");
        assert!(out.contains("role viewer"), "{out}");
    }
    // Drive: refused, with the capability named, and a failing exit code.
    for verb in ["send", "spawn", "split"] {
        let (ok, out) = scratch.run(&["devices", "authorize", &key, "--verb", verb]);
        assert!(!ok, "a viewer must not {verb}: {out}");
        assert!(out.contains("needs Control"), "{out}");
    }
    // The JSON form is machine-readable for scripts and CI.
    let (ok, out) = scratch.run(&["devices", "authorize", &key, "--verb", "send", "--json"]);
    assert!(!ok, "{out}");
    let value: serde_json::Value = serde_json::from_str(out.trim()).expect("json");
    assert_eq!(value["allowed"], serde_json::json!(false));
    assert!(
        value["reason"]
            .as_str()
            .is_some_and(|r| r.contains("needs Control")),
        "{out}"
    );

    // An owner holds both capabilities.
    let owner = scratch.other_identity("laptop");
    let owner_key = scratch.public_key_of(&owner);
    scratch.run(&[
        "devices", "issue", "--name", "laptop", "--role", "owner", "--key", &owner_key,
    ]);
    for verb in ["read", "send", "spawn"] {
        let (ok, out) = scratch.run(&["devices", "authorize", &owner_key, "--verb", verb]);
        assert!(ok, "an owner must be allowed to {verb}: {out}");
    }

    // A key with no certificate is refused before the policy is consulted.
    let stranger = scratch.other_identity("stranger");
    let stranger_key = scratch.public_key_of(&stranger);
    let (ok, out) = scratch.run(&["devices", "authorize", &stranger_key, "--verb", "read"]);
    assert!(!ok, "{out}");
    assert!(out.contains("has no certificate"), "{out}");
}

#[test]
fn key_files_are_owner_only_and_loose_ones_are_refused() {
    let scratch = Scratch::new("permissions");
    let key = scratch.public_key_of(&scratch.identity());
    scratch.run(&[
        "devices", "issue", "--name", "self", "--role", "owner", "--key", &key,
    ]);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [
            scratch.identity().join("identity").join("root.key"),
            scratch.identity().join("identity").join("device.key"),
        ] {
            let mode = std::fs::metadata(&path)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "{} must be owner-only", path.display());
        }
        // A world-readable root key is refused loudly instead of being trusted.
        let root_key = scratch.identity().join("identity").join("root.key");
        std::fs::set_permissions(&root_key, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        let (ok, out) = scratch.run(&["devices", "list"]);
        assert!(!ok, "a world-readable root key must be refused: {out}");
        assert!(out.contains("permissions"), "{out}");
    }
}

#[test]
fn bad_arguments_are_refused_with_a_usage_line() {
    let scratch = Scratch::new("args");
    // Missing --role.
    let (ok, out) = scratch.run(&["devices", "issue", "--name", "x", "--key", &"a".repeat(64)]);
    assert!(!ok, "{out}");
    assert!(out.contains("missing --role"), "{out}");
    // An all-zero key is a real curve point but a weak (small-order) one:
    // pinning it would authorize anyone who can present it.
    let (ok, out) = scratch.run(&[
        "devices",
        "issue",
        "--name",
        "x",
        "--role",
        "owner",
        "--key",
        &"0".repeat(64),
    ]);
    assert!(!ok, "{out}");
    assert!(out.contains("weak public key"), "{out}");
    // Not a device id.
    let (ok, out) = scratch.run(&["devices", "revoke", "nonsense"]);
    assert!(!ok, "{out}");
    assert!(out.contains("not a device id"), "{out}");
    // Unknown subcommand.
    let (ok, out) = scratch.run(&["devices", "wibble"]);
    assert!(!ok, "{out}");
    assert!(out.contains("unknown subcommand"), "{out}");
}
