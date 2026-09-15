//! T-0105: the **handoff** path never promotes a pending update.
//!
//! ## The rule this file pins
//!
//! A cold start (`Daemon::serve`) promotes a pending staged update and re-execs
//! into it. The *incoming* side of a live handoff (`Daemon::serve_inherited`) must
//! not: that process has just adopted the listener, the lock and the live panes
//! from the outgoing daemon, and promoting there would swap the binary under a
//! live cut and re-exec a process holding adopted panes — every agent's PTY would
//! be dropped by a start that was supposed to cost nobody their work.
//!
//! ## Why the test is shaped like this
//!
//! The guard is structural (only `serve` calls the promotion), so the test has to
//! be *behavioral* to be worth anything: it stages a genuinely promotable update,
//! drives the inherited path against it, and shows the pending state untouched —
//! marker still there, stage still there, installed binary unchanged, no rollback
//! slot created.
//!
//! The control that makes it non-vacuous is the assertion that the staged artifact
//! is a runnable binary reporting the version the marker records: that is exactly
//! what `deferred::promote_in` requires, so a state this test leaves alone is a
//! state the cold path would have promoted (the unit tests in
//! `src/daemon.rs::start_update_tests` drive the same shape through
//! `Daemon::start_decision` and get `Reexec`). Without that, the test would pass
//! on an unpromotable state and prove nothing.
//!
//! `ARREO_STATE_DIR` is pointed at the scratch directory because the inherited
//! path resolves the state directory the way the daemon does — from the
//! environment. This file holds one test, so nothing in this process races that
//! variable, and a test that wrote the real state directory would corrupt the
//! machine it runs on.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// A scratch directory under the workspace's `target/test-scratch`: nothing is
/// written outside the build tree (`/tmp` is the machine's, not ours).
fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the workspace root is two levels above this crate")
        .join("target")
        .join("test-scratch")
        .join("deferred-start")
        .join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the scratch directory");
    dir
}

/// A stand-in server: answers `--version` with `version`, and **fails on anything
/// else**.
///
/// The failing arm is load-bearing. A start path that wrongly execs this fixture
/// (the handoff path must not) would otherwise replace the test process with a
/// script that exits 0 — a green run for a red mistake. Exiting 1 turns that into
/// a failed test binary, which is what makes this file's mutation guard real.
fn server(path: &Path, version: &str) {
    use std::os::unix::fs::PermissionsExt;
    let body = format!(
        "#!/bin/sh\ncase \"$1\" in\n  --version) echo \"arreo-server {version}\" ;;\n  *) exit 1 ;;\nesac\nexit 0\n"
    );
    std::fs::write(path, body).expect("write the stand-in");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .expect("make the stand-in executable");
}

#[test]
fn the_handoff_path_never_promotes_a_pending_update() {
    use arreo_core::update::{deferred, verify_runs};

    let dir = scratch("handoff");
    let state = dir.join("state");
    let socket = dir.join("arreo.sock");
    let current = dir.join("arreo-server");
    server(&current, "0.1.0");
    let artifact = dir.join("server-new");
    server(&artifact, "0.3.0");
    let pending = deferred::stage_next_in(
        &state,
        &artifact,
        &current,
        "arreo-server 0.1.0",
        "arreo-server 0.3.0",
    )
    .expect("stage the artifact");
    std::env::set_var("ARREO_STATE_DIR", &state);

    // The control: the staged artifact is runnable and reports the version the
    // marker records — the two facts `promote_in` re-proves before it swaps. This
    // is a state the cold path promotes (see `start_update_tests`), so a path that
    // leaves it alone is a path that refused to promote.
    assert_eq!(
        verify_runs(&pending.staged_path())
            .expect("the stage runs")
            .trim(),
        "arreo-server 0.3.0"
    );

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    runtime.block_on(async {
        let listener = tokio::net::UnixListener::bind(&socket).expect("bind the socket");
        let lock_path = arreo_server::persist::lock_path_for(&socket);
        let lock = arreo_core::lock::ExclusiveLock::acquire(&lock_path).expect("take the lock");
        let daemon = Arc::new(arreo_server::Daemon::new(&socket));
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<Result<(), String>>();
        let serving = {
            let daemon = Arc::clone(&daemon);
            tokio::spawn(async move {
                daemon
                    .serve_inherited(listener, lock, Vec::new(), Some(ready_tx))
                    .await
            })
        };
        // Readiness is the accept loop owning the listener: everything the
        // inherited path does to a pending update has happened by the time it
        // reports this.
        let ready = tokio::time::timeout(Duration::from_secs(10), ready_rx)
            .await
            .expect("the inherited path reports readiness")
            .expect("the readiness channel is not dropped");
        ready.expect("the inherited path serves");
        serving.abort();
        let _ = serving.await;
    });

    assert!(
        deferred::read_marker_in(&state)
            .expect("read the marker")
            .is_some(),
        "the handoff path must leave the marker pending: the update is not its to land"
    );
    assert!(
        pending.staged_path().exists(),
        "and the stage: a promotion would have renamed it into the install path"
    );
    assert_eq!(
        verify_runs(&current)
            .expect("the installed binary runs")
            .trim(),
        "arreo-server 0.1.0",
        "the installed binary is untouched"
    );
    assert!(
        !arreo_core::update::prev_path(&current).exists(),
        "and no rollback slot was created, because nothing was promoted"
    );
}
