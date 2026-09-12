//! T-0071: exactly one daemon serves a socket.
//!
//! ## The defect this file pins
//!
//! `Daemon::serve` decided the socket was dead by *probing* it (`connect`), then
//! `remove_file`d it and bound its own listener. Those three steps were not
//! atomic, so two daemons starting together both probed a not-yet-bound path,
//! both unlinked, and the second one **removed the first's listener** and bound
//! its own. The machine then had two live daemons, one of them unreachable.
//!
//! Measured before the fix: 1 round in 12 of eight simultaneous starts produced
//! two live daemons — rare, silent, and it survives because nothing owns the path.
//!
//! ## Why the test is shaped like this
//!
//! A race is not a good regression test: a racing test would pass ~11 times in 12
//! before the fix, which is a test that reports "fine" about a broken machine.
//! So the test asserts the **rule** the fix introduces, in the form that is
//! deterministic: *only the holder of the socket's lock may serve it.*
//!
//! The lock is taken here by the test process itself and the socket file is left
//! **absent**, which is exactly the state the racing daemons both saw. Before the
//! fix a daemon would probe (nothing there), bind, and serve happily; after it,
//! the daemon refuses. The failure mode is therefore reproduced rather than
//! approximated, and the test cannot pass by accident.

use std::path::PathBuf;
use std::time::Duration;

/// The daemon binary, beside this test's own executable.
fn server_binary() -> PathBuf {
    std::env::current_exe()
        .expect("test exe")
        .parent()
        .expect("deps dir")
        .parent()
        .expect("debug dir")
        .join("arreo-server")
}

fn temp_socket(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "arreo-instance-{name}-{}-{:?}.sock",
        std::process::id(),
        std::thread::current().id()
    ))
}

/// Wait up to `limit` for the child to exit, without ever blocking past it.
fn exited_within(child: &mut std::process::Child, limit: Duration) -> bool {
    let deadline = std::time::Instant::now() + limit;
    while std::time::Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => panic!("try_wait: {e}"),
        }
    }
    false
}

/// A child that is killed when the test ends, so a failing assertion cannot leave
/// a daemon holding a port.
struct Guard(std::process::Child);

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// **The regression test.** While another process holds the socket's lock, a
/// daemon must refuse to serve it — even though the socket file does not exist
/// and a probe would find nothing.
#[test]
fn a_daemon_refuses_a_socket_whose_lock_another_holder_has() {
    let socket = temp_socket("held");
    let lock_path = arreo_server::persist::lock_path_for(&socket);
    let held = arreo_core::lock::ExclusiveLock::acquire(&lock_path).expect("take the lock");

    // The exact precondition of the race: no socket at the path, so a liveness
    // probe says "dead" and the old code would have bound it.
    assert!(
        !socket.exists(),
        "the socket must be absent so the probe would say 'dead'"
    );

    let mut child = Guard(
        std::process::Command::new(server_binary())
            .arg("--socket")
            .arg(&socket)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("arreo-server runs"),
    );

    let exited = exited_within(&mut child.0, Duration::from_secs(10));
    if !exited {
        // Before the fix this is where the test fails: the daemon is happily
        // serving a socket another process owns.
        panic!("the daemon served a socket whose lock is held by another process");
    }

    let status = child.0.wait().expect("wait");
    assert!(
        !status.success(),
        "a refused start must be a failure, not a silent no-op"
    );
    assert!(
        !socket.exists(),
        "a refused daemon must not leave a socket behind"
    );
    drop(held);
}

/// The other half of the rule: once the holder is gone, the same socket is
/// servable again. Without this, a fix that refused *always* would pass the test
/// above.
#[test]
fn the_socket_is_servable_once_the_holder_releases_it() {
    let socket = temp_socket("released");
    let lock_path = arreo_server::persist::lock_path_for(&socket);

    {
        let _held = arreo_core::lock::ExclusiveLock::acquire(&lock_path).expect("take");
    } // released

    let mut child = Guard(
        std::process::Command::new(server_binary())
            .arg("--socket")
            .arg(&socket)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("arreo-server runs"),
    );

    // It must *not* exit: it is serving. Readiness is the socket accepting, which
    // is the only thing that proves a listener exists.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut serving = false;
    while std::time::Instant::now() < deadline {
        if socket.exists() && std::os::unix::net::UnixStream::connect(&socket).is_ok() {
            serving = true;
            break;
        }
        if let Ok(Some(status)) = child.0.try_wait() {
            panic!("daemon exited instead of serving: {status}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(serving, "the daemon never came up on a free socket");

    // And a *second* daemon on the same socket, with the first genuinely serving,
    // is refused — the everyday form of the rule.
    let mut second = Guard(
        std::process::Command::new(server_binary())
            .arg("--socket")
            .arg(&socket)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("arreo-server runs"),
    );
    assert!(
        exited_within(&mut second.0, Duration::from_secs(10)),
        "a second daemon must refuse a socket that is being served"
    );
    // What this can prove from outside is that the path still accepts: the
    // refused daemon must not have unlinked the serving daemon's socket on its way
    // out. (Which daemon answers is not observable — `PaneInfo` carries no pid —
    // so this asserts the effect, not the identity.)
    assert!(
        std::os::unix::net::UnixStream::connect(&socket).is_ok(),
        "the refused daemon must leave the serving one's socket reachable"
    );
}
