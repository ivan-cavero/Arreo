//! The `arreo-relay` binary: flags, listeners, and the mailbox a phone reaches.
//!
//! This exists for two reasons. First, the pairing mailbox is the one service a
//! device talks to *before* it is trusted, so "does the binary actually serve
//! what the library does" deserves its own check rather than an assumption.
//! Second, referencing `CARGO_BIN_EXE_arreo-relay` here is what makes
//! `cargo test --workspace` build the binary — which the cross-process pairing
//! tests in `arreo-cli` need in order to spawn it.

use arreo_core::pairing::{MailboxAddr, MailboxClient, Slot};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn relay_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_arreo-relay"))
}

/// A spawned relay, killed when the test ends.
struct Relay(Child);

impl Relay {
    fn start(args: &[&str]) -> Self {
        let child = Command::new(relay_binary())
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("the relay starts");
        Self(child)
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_for_unix(socket: &std::path::Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while std::os::unix::net::UnixStream::connect(socket).is_err() {
        assert!(
            Instant::now() < deadline,
            "the relay never bound {}",
            socket.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A free TCP port, released immediately before use (the standard race is
/// acceptable for a test, and the retry below covers a lost race).
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
    listener.local_addr().expect("local addr").port()
}

#[test]
fn the_binary_serves_the_mailbox_over_a_unix_socket() {
    let dir = std::env::temp_dir().join(format!("arreo-relay-bin-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let socket = dir.join("relay.sock");
    let _relay = Relay::start(&["--pairing-socket", socket.to_str().expect("utf-8")]);
    wait_for_unix(&socket);

    // The client the product uses, against the binary the product ships: a
    // session that opens, takes one flight, and is then spent.
    let client = MailboxClient::new(MailboxAddr::Unix(socket.clone()));
    client
        .open("session-from-the-binary-test", Duration::from_secs(30))
        .expect("the binary's mailbox opens a session");
    client
        .put("session-from-the-binary-test", Slot::A, b"flight")
        .expect("and accepts a flight");
    assert_eq!(
        client
            .get("session-from-the-binary-test", Slot::A)
            .expect("and returns it"),
        Some(b"flight".to_vec())
    );
    client
        .burn("session-from-the-binary-test")
        .expect("and closes it");
    assert!(
        client
            .open("session-from-the-binary-test", Duration::from_secs(30))
            .is_err(),
        "a spent session was reopened"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// The network shape a phone uses: the same mailbox over TCP, reached the way
/// the QR's `host:port` mailbox is.
#[test]
fn the_binary_serves_the_mailbox_over_tcp() {
    let mut last_error = String::new();
    for _ in 0..5 {
        let port = free_port();
        let addr = format!("127.0.0.1:{port}");
        let _relay = Relay::start(&["--pairing-tcp", &addr]);
        // Wait for the listener, then speak the protocol the phone speaks.
        let client = MailboxClient::new(MailboxAddr::Tcp(addr.clone()));
        let mut opened = false;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            match client.open("tcp-session", Duration::from_secs(30)) {
                Ok(()) => {
                    opened = true;
                    break;
                }
                Err(e) => {
                    last_error = e.to_string();
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
        if !opened {
            continue; // lost the port race; try another
        }
        client
            .put("tcp-session", Slot::B, b"flight-over-tcp")
            .expect("a flight over TCP");
        assert_eq!(
            client.get("tcp-session", Slot::B).expect("read back"),
            Some(b"flight-over-tcp".to_vec())
        );
        return;
    }
    panic!("the relay never served TCP: {last_error}");
}

#[test]
fn the_binary_refuses_to_start_with_nothing_to_serve() {
    let output = Command::new(relay_binary())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .expect("the relay runs");
    assert!(!output.status.success(), "a listenerless relay exited 0");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("nothing to serve"),
        "the refusal must say why: {stderr}"
    );
    // And it tells the operator what the flags are.
    let help = Command::new(relay_binary())
        .arg("--help")
        .output()
        .expect("the relay runs");
    let text = String::from_utf8_lossy(&help.stdout);
    assert!(text.contains("--pairing-socket"), "{text}");
    assert!(text.contains("--pairing-tcp"), "{text}");
}

#[test]
fn an_unknown_flag_is_refused() {
    let output = Command::new(relay_binary())
        .arg("--wibble")
        .output()
        .expect("the relay runs");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unknown flag"),
        "{:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}
