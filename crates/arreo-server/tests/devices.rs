//! T-0025 daemon-side device authority, tested through the real `arreo-server`
//! binary.
//!
//! Two claims need a real process rather than a function call:
//! 1. boot *creates* the authority (root key 0600, in a 0700 dir) and says so;
//! 2. boot *refuses to serve* when the root key exists but is unusable — a
//!    daemon that silently minted a new root would invalidate every paired
//!    device without telling anyone.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn server() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_arreo-server"))
}

struct Scratch {
    dir: PathBuf,
    socket: PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "arreo-server-devices-{tag}-{}-{:?}",
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

    fn root_key(&self) -> PathBuf {
        self.dir.join("identity").join("root.key")
    }

    /// Start the daemon and wait until it either binds or exits. Returns
    /// (did it bind, combined output, child).
    fn start(&self, timeout: Duration) -> (bool, String, std::process::Child) {
        let mut child = Command::new(server())
            .arg("--socket")
            .arg(&self.socket)
            .env("ARREO_IDENTITY_DIR", &self.dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("daemon starts");
        let deadline = Instant::now() + timeout;
        let bound = loop {
            if std::os::unix::net::UnixStream::connect(&self.socket).is_ok() {
                break true;
            }
            if child.try_wait().expect("wait").is_some() {
                break false;
            }
            if Instant::now() >= deadline {
                break false;
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        (bound, String::new(), child)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn boot_bootstraps_the_root_key_and_serves() {
    let scratch = Scratch::new("boot");
    let (bound, _, mut child) = scratch.start(Duration::from_secs(10));
    assert!(bound, "the daemon must bind its socket");

    // The authority appeared, with the permissions the threat model assumes.
    let root_key = scratch.root_key();
    let metadata = std::fs::metadata(&root_key).expect("root key written at boot");
    assert!(metadata.len() >= 64, "root key looks empty");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            metadata.permissions().mode() & 0o777,
            0o600,
            "the root key must be owner-only"
        );
        let dir_mode = std::fs::metadata(root_key.parent().expect("parent"))
            .expect("identity dir")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700, "the identity dir must be owner-only");
    }

    // A second boot reuses the same root instead of minting a new one.
    let first = std::fs::read_to_string(&root_key).expect("read root key");
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_file(&scratch.socket);
    let (bound, _, mut child) = scratch.start(Duration::from_secs(10));
    assert!(bound);
    let second = std::fs::read_to_string(&root_key).expect("read root key");
    assert_eq!(first, second, "the root key must be stable across restarts");
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn a_broken_root_key_refuses_to_serve_and_is_not_replaced() {
    let scratch = Scratch::new("broken");
    std::fs::create_dir_all(scratch.dir.join("identity")).expect("identity dir");
    let root_key = scratch.root_key();
    std::fs::write(&root_key, "this is not a key").expect("write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root_key, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    }

    let mut child = Command::new(server())
        .arg("--socket")
        .arg(&scratch.socket)
        .env("ARREO_IDENTITY_DIR", &scratch.dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("daemon starts");
    let status = child.wait().expect("daemon exits");
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    assert!(
        !status.success(),
        "a daemon with an unusable root key must not serve: {stderr}"
    );
    assert!(
        stderr.contains("device identity unavailable"),
        "the refusal must say why: {stderr}"
    );
    // It did not serve, and it did not overwrite the operator's file.
    assert!(
        !Path::new(&scratch.socket).exists(),
        "the socket was created"
    );
    assert_eq!(
        std::fs::read_to_string(&root_key)
            .expect("still there")
            .trim(),
        "this is not a key",
        "a broken root key must never be replaced silently"
    );
}

#[cfg(unix)]
#[test]
fn a_world_readable_root_key_is_refused_at_boot() {
    let scratch = Scratch::new("loose");
    std::fs::create_dir_all(scratch.dir.join("identity")).expect("identity dir");
    let root_key = scratch.root_key();
    std::fs::write(&root_key, "ab".repeat(32)).expect("write a well-formed key");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root_key, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    }
    let mut child = Command::new(server())
        .arg("--socket")
        .arg(&scratch.socket)
        .env("ARREO_IDENTITY_DIR", &scratch.dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("daemon starts");
    let status = child.wait().expect("daemon exits");
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    assert!(
        !status.success(),
        "a world-readable root key must stop boot"
    );
    assert!(
        stderr.contains("permissions"),
        "the refusal must name the permission problem: {stderr}"
    );
}
