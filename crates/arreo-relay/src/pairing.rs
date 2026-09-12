//! The pairing mailbox (T-0024): four write-once slots, single-use sessions.
//!
//! One sentence: the relay holds opaque blobs for two minutes and cannot read
//! them — its whole job is to enforce *when* and *how often* a session may be
//! used, which is what turns a 32-bit code into a one-guess secret.
//!
//! The rules, each one load-bearing:
//! - **Single use.** A session id can be opened once. A burned or expired id
//!   is refused forever (within this process), so a captured transcript cannot
//!   be replayed into a *fresh* session by re-using the id.
//! - **Write-once slots.** Nobody — not the peer, not the relay's other
//!   clients, not a network attacker — can replace a flight after it is
//!   published. A forged early write costs a failed confirmation (a DoS), it
//!   cannot become an impersonation, because the confirmation MAC is keyed by
//!   a secret the attacker does not have.
//! - **Bounded lifetime.** `ttl_secs` is fixed at open and enforced here, so
//!   the relay's clock is the clock of record: both parties see the same
//!   window close.
//!
//! Honest limits (v0, stated rather than implied): the store is **in memory** —
//! a relay restart mid-pairing fails that pairing loudly and the human re-runs
//! `arreo pair` (durability is the inbox work, T-0030); the burned-id memory is
//! capped (see `MAX_REMEMBERED_BURNS`); and the listener is single-threaded
//! with a read timeout, which is a denial-of-service surface on a *local*
//! socket — admission control belongs to the relay's network work (T-0029).

use arreo_core::pairing::{MailboxRequest, MailboxResponse, Slot};
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
// Unix-only, and gated as such (T-0062): this crate must compile for
// `x86_64-pc-windows-msvc` because the workspace is built and tested there, and
// `std::os::unix` does not exist on Windows. The *mailbox rules above are
// platform-independent* — the TCP listener below serves them everywhere; only
// the unix-socket entry points are gated, exactly as `arreo_core`'s client half
// gates its own (`crates/arreo-core/src/pairing/wire.rs`).
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(unix)]
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How many used session ids are remembered. Replaying an id is only
/// interesting within the life of a pairing, so a bounded ring is enough for
/// v0; the durable inbox (T-0030) owns the real answer.
pub const MAX_REMEMBERED_BURNS: usize = 4096;

/// Largest request frame accepted, in bytes. SPAKE2 flights are ~40 bytes and
/// certificates ~300, so this is generous by two orders of magnitude while
/// still bounding what one client can make the relay allocate.
pub const MAX_FRAME_BYTES: usize = 64 * 1024;

/// How long a client may take to send its one request.
pub const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// One live pairing session: four slots and a fixed expiry.
#[derive(Debug)]
struct Session {
    slots: [Option<Vec<u8>>; 4],
    expires_at: Instant,
}

impl Session {
    fn slot_index(slot: Slot) -> usize {
        match slot {
            Slot::A => 0,
            Slot::B => 1,
            Slot::C => 2,
            Slot::D => 3,
        }
    }
}

#[derive(Debug, Default)]
struct State {
    live: HashMap<String, Session>,
    /// Burned or expired ids. `order` makes the cap FIFO rather than random.
    used: HashMap<String, ()>,
    used_order: VecDeque<String>,
}

impl State {
    fn remember_used(&mut self, session: &str) {
        if self.used.insert(session.to_string(), ()).is_none() {
            self.used_order.push_back(session.to_string());
            while self.used_order.len() > MAX_REMEMBERED_BURNS {
                if let Some(oldest) = self.used_order.pop_front() {
                    self.used.remove(&oldest);
                }
            }
        }
    }

    /// Drop expired sessions. Called on every request so a forgotten pairing
    /// cannot pin memory, and so an expired id is remembered as used.
    fn sweep(&mut self, now: Instant) {
        let expired: Vec<String> = self
            .live
            .iter()
            .filter(|(_, session)| session.expires_at <= now)
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            self.live.remove(&id);
            self.remember_used(&id);
        }
    }
}

/// The mailbox: the relay's pairing state.
#[derive(Debug, Default)]
pub struct Mailbox {
    state: Mutex<State>,
}

impl Mailbox {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply one request. The reply is the caller's answer — including refusals,
    /// which are values (`ok: false`) rather than transport errors, so a client
    /// can tell "the relay said no" from "the relay is gone".
    #[must_use]
    pub fn handle(&self, request: &MailboxRequest) -> MailboxResponse {
        let now = Instant::now();
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        state.sweep(now);
        match request {
            MailboxRequest::Open { session, ttl_secs } => {
                if state.live.contains_key(session) {
                    return MailboxResponse::error("session is already open");
                }
                if state.used.contains_key(session) {
                    return MailboxResponse::error("session has already been used");
                }
                if session.is_empty() || session.len() > 128 {
                    return MailboxResponse::error("session id must be 1..=128 characters");
                }
                state.live.insert(
                    session.clone(),
                    Session {
                        slots: [None, None, None, None],
                        expires_at: now + Duration::from_secs((*ttl_secs).max(1)),
                    },
                );
                MailboxResponse::ok()
            }
            MailboxRequest::Put {
                session,
                slot,
                payload,
            } => {
                if payload.len() > MAX_FRAME_BYTES {
                    return MailboxResponse::error("flight is too large");
                }
                let Some(entry) = state.live.get_mut(session) else {
                    return MailboxResponse::error(self.explain_missing(&state, session, now));
                };
                if entry.expires_at <= now {
                    return MailboxResponse::error("session has expired");
                }
                let index = Session::slot_index(*slot);
                if entry.slots[index].is_some() {
                    // Write-once: the first flight in a slot wins, so nobody can
                    // replace one after the fact.
                    return MailboxResponse::error("slot already holds a flight");
                }
                entry.slots[index] = Some(payload.clone().into_bytes());
                MailboxResponse::ok()
            }
            MailboxRequest::Get { session, slot } => {
                let Some(entry) = state.live.get(session) else {
                    return MailboxResponse::error(self.explain_missing(&state, session, now));
                };
                if entry.expires_at <= now {
                    return MailboxResponse::error("session has expired");
                }
                let payload = entry.slots[Session::slot_index(*slot)]
                    .as_ref()
                    .map(|bytes| String::from_utf8_lossy(bytes).to_string());
                MailboxResponse::with_payload(payload)
            }
            MailboxRequest::Burn { session } => {
                if state.live.remove(session).is_some() {
                    state.remember_used(session);
                    return MailboxResponse::ok();
                }
                if state.used.contains_key(session) {
                    // Idempotent: both sides burn, and a retry must not fail.
                    return MailboxResponse::ok();
                }
                MailboxResponse::error("no such session")
            }
            MailboxRequest::Status { session } => {
                if let Some(entry) = state.live.get(session) {
                    let left = entry.expires_at.saturating_duration_since(now);
                    return MailboxResponse::with_ttl(left.as_millis() as u64);
                }
                MailboxResponse::error(self.explain_missing(&state, session, now))
            }
        }
    }

    /// Why a session is not live: never seen, or used (burned or expired). The
    /// distinction is what lets the CLI say "that pairing is over" instead of
    /// "unknown session".
    fn explain_missing(&self, state: &State, session: &str, now: Instant) -> String {
        let _ = now;
        if state.used.contains_key(session) {
            "session is no longer available (used or expired)".to_string()
        } else {
            "no such session".to_string()
        }
    }
}

/// Serve the mailbox on a Unix socket, one request per connection, forever.
///
/// Blocking and single-request-at-a-time on purpose: the mailbox carries four
/// small frames between two clients, and a simple loop is auditable. The read
/// timeout bounds how long one stalled client can hold it.
#[cfg(unix)]
pub fn serve_unix(path: &Path, mailbox: Arc<Mailbox>) -> std::io::Result<()> {
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
                let _ = stream.set_write_timeout(Some(READ_TIMEOUT));
                serve_connection(stream, &mailbox);
            }
            Err(e) => eprintln!("arreo-relay: pairing accept failed: {e}"),
        }
    }
    Ok(())
}

/// Serve the same mailbox over TCP (`host:port`) — the shape a phone reaches
/// through a network. The flights are public by design, so plain TCP adds no
/// confidentiality requirement; the code authenticates the exchange.
pub fn serve_tcp(addr: &str, mailbox: Arc<Mailbox>) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr)?;
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
                let _ = stream.set_write_timeout(Some(READ_TIMEOUT));
                serve_connection(stream, &mailbox);
            }
            Err(e) => eprintln!("arreo-relay: pairing accept failed: {e}"),
        }
    }
    Ok(())
}

/// One request, one reply, close.
fn serve_connection<S: std::io::Read + Write>(stream: S, mailbox: &Mailbox) {
    let mut reader = BufReader::new(stream);
    let response = match read_frame(&mut reader) {
        Ok(Some(line)) => match serde_json::from_str::<MailboxRequest>(&line) {
            Ok(request) => mailbox.handle(&request),
            Err(e) => MailboxResponse::error(format!("malformed request: {e}")),
        },
        Ok(None) => MailboxResponse::error("empty request"),
        Err(e) => MailboxResponse::error(format!("cannot read request: {e}")),
    };
    let payload = match serde_json::to_string(&response) {
        Ok(payload) => payload,
        Err(e) => {
            eprintln!("arreo-relay: cannot encode reply: {e}");
            return;
        }
    };
    let mut stream = reader.into_inner();
    let _ = stream.write_all(payload.as_bytes());
    let _ = stream.write_all(b"\n");
    let _ = stream.flush();
}

/// Read one newline-terminated frame, refusing anything past [`MAX_FRAME_BYTES`].
/// `Ok(None)` means the peer closed without sending anything.
fn read_frame<R: BufRead>(reader: &mut R) -> std::io::Result<Option<String>> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match reader.read_exact(&mut byte) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return if line.is_empty() {
                    Ok(None)
                } else {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "frame ended without a newline",
                    ))
                };
            }
            Err(e) => return Err(e),
        }
        if byte[0] == b'\n' {
            break;
        }
        if line.len() >= MAX_FRAME_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "frame is larger than the mailbox accepts",
            ));
        }
        line.push(byte[0]);
    }
    String::from_utf8(line)
        .map(Some)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Connect, send one request, read the reply (used by the relay's own tests
/// and by anything scripting the mailbox).
#[cfg(unix)]
pub fn request_unix(path: &Path, request: &MailboxRequest) -> std::io::Result<MailboxResponse> {
    let stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    stream.set_write_timeout(Some(READ_TIMEOUT))?;
    let line = serde_json::to_string(request).unwrap_or_default();
    let mut reader = BufReader::new(stream);
    reader.get_mut().write_all(line.as_bytes())?;
    reader.get_mut().write_all(b"\n")?;
    reader.get_mut().flush()?;
    let mut reply = String::new();
    reader.read_line(&mut reply)?;
    serde_json::from_str(&reply)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mailbox() -> Mailbox {
        Mailbox::new()
    }

    fn open(mailbox: &Mailbox, session: &str, ttl_secs: u64) -> MailboxResponse {
        mailbox.handle(&MailboxRequest::Open {
            session: session.to_string(),
            ttl_secs,
        })
    }

    fn put(mailbox: &Mailbox, session: &str, slot: Slot, payload: &str) -> MailboxResponse {
        mailbox.handle(&MailboxRequest::Put {
            session: session.to_string(),
            slot,
            payload: payload.to_string(),
        })
    }

    fn get(mailbox: &Mailbox, session: &str, slot: Slot) -> MailboxResponse {
        mailbox.handle(&MailboxRequest::Get {
            session: session.to_string(),
            slot,
        })
    }

    #[test]
    fn a_session_carries_four_flights_once_each() {
        let mailbox = mailbox();
        assert!(open(&mailbox, "s1", 300).ok);
        // Empty slots read as "not yet", not as an error: that is how the peer
        // waits without guessing.
        let empty = get(&mailbox, "s1", Slot::A);
        assert!(empty.ok && empty.payload.is_none());
        for (slot, payload) in [
            (Slot::A, "flight-a"),
            (Slot::B, "flight-b"),
            (Slot::C, "hello"),
            (Slot::D, "cert"),
        ] {
            assert!(
                put(&mailbox, "s1", slot, payload).ok,
                "{slot:?} was refused"
            );
            let read = get(&mailbox, "s1", slot);
            assert_eq!(read.payload.as_deref(), Some(payload));
        }
        // Write-once: the second write to a slot is refused, whatever it is.
        let overwrite = put(&mailbox, "s1", Slot::A, "forged");
        assert!(!overwrite.ok, "a slot accepted a second flight");
        assert_eq!(
            get(&mailbox, "s1", Slot::A).payload.as_deref(),
            Some("flight-a"),
            "the first flight must win"
        );
    }

    #[test]
    fn a_session_id_is_single_use() {
        let mailbox = mailbox();
        assert!(open(&mailbox, "s1", 300).ok);
        // Opening the same id again while live is refused.
        assert!(!open(&mailbox, "s1", 300).ok, "a live session was reopened");
        // After a burn it stays refused: this is what makes a captured
        // transcript useless against a fresh attempt.
        assert!(
            mailbox
                .handle(&MailboxRequest::Burn {
                    session: "s1".into()
                })
                .ok
        );
        assert!(
            !open(&mailbox, "s1", 300).ok,
            "a burned session was reopened"
        );
        // Burn is idempotent (both sides burn, one may retry).
        assert!(
            mailbox
                .handle(&MailboxRequest::Burn {
                    session: "s1".into()
                })
                .ok
        );
        // Nothing in a burned session can be read or written any more.
        assert!(!put(&mailbox, "s1", Slot::A, "x").ok);
        assert!(!get(&mailbox, "s1", Slot::A).ok);
        // A *different* session is unaffected.
        assert!(open(&mailbox, "s2", 300).ok);
    }

    #[test]
    fn expiry_closes_the_window_and_retires_the_id() {
        let mailbox = mailbox();
        assert!(open(&mailbox, "short", 1).ok);
        assert!(put(&mailbox, "short", Slot::A, "flight").ok);
        // Wait out the 1 s TTL rather than sleeping for a real pairing window.
        std::thread::sleep(Duration::from_millis(1100));
        for request in [
            MailboxRequest::Get {
                session: "short".into(),
                slot: Slot::A,
            },
            MailboxRequest::Put {
                session: "short".into(),
                slot: Slot::B,
                payload: "x".into(),
            },
            MailboxRequest::Status {
                session: "short".into(),
            },
        ] {
            let response = mailbox.handle(&request);
            assert!(!response.ok, "{request:?} was served after expiry");
            assert!(
                response
                    .error
                    .as_deref()
                    .unwrap_or_default()
                    .contains("no longer available"),
                "{response:?}"
            );
        }
        // The expired id is remembered as used, so it cannot be reopened.
        assert!(!open(&mailbox, "short", 300).ok);
        // Its payloads are gone from memory.
        let state = mailbox.state.lock().expect("lock");
        assert!(state.live.is_empty());
    }

    #[test]
    fn status_reports_the_time_left_and_unknown_sessions_are_named() {
        let mailbox = mailbox();
        assert!(open(&mailbox, "s1", 60).ok);
        let status = mailbox.handle(&MailboxRequest::Status {
            session: "s1".into(),
        });
        let ttl = status.ttl_ms.expect("a live session has a ttl");
        assert!((55_000..=60_000).contains(&ttl), "ttl was {ttl} ms");
        // An id that was never opened says exactly that.
        let unknown = mailbox.handle(&MailboxRequest::Status {
            session: "nope".into(),
        });
        assert!(!unknown.ok);
        assert_eq!(unknown.error.as_deref(), Some("no such session"));
        // Burning something that never existed is not silently "fine".
        assert!(
            !mailbox
                .handle(&MailboxRequest::Burn {
                    session: "nope".into()
                })
                .ok
        );
    }

    #[test]
    fn nonsense_requests_are_refused_without_touching_state() {
        let mailbox = mailbox();
        // An empty or absurd session id never becomes a session.
        assert!(!open(&mailbox, "", 300).ok);
        assert!(!open(&mailbox, &"x".repeat(129), 300).ok);
        // A flight larger than the cap is refused (memory is bounded).
        let huge = "A".repeat(MAX_FRAME_BYTES + 1);
        assert!(open(&mailbox, "s", 300).ok);
        let response = put(&mailbox, "s", Slot::A, &huge);
        assert!(!response.ok);
        assert!(get(&mailbox, "s", Slot::A).payload.is_none());
        // A ttl of zero is clamped up, not treated as "expired already".
        assert!(open(&mailbox, "zero", 0).ok);
        assert!(
            mailbox
                .handle(&MailboxRequest::Status {
                    session: "zero".into()
                })
                .ok
        );
    }

    #[test]
    fn the_burn_memory_is_bounded_but_keeps_the_recent_ids() {
        let mut state = State::default();
        for index in 0..(MAX_REMEMBERED_BURNS + 10) {
            state.remember_used(&format!("s{index}"));
        }
        assert_eq!(state.used.len(), MAX_REMEMBERED_BURNS);
        // The newest id is remembered, the oldest has been forgotten — a
        // bounded ring, not a leak.
        assert!(state
            .used
            .contains_key(&format!("s{}", MAX_REMEMBERED_BURNS + 9)));
        assert!(!state.used.contains_key("s0"));
    }

    /// The store rules are tested above; this drives the same rules through the
    /// real socket path a client uses, so framing bugs cannot hide behind a
    /// direct `handle` call.
    ///
    /// Gated like the entry point it exercises (T-0062): the unix socket, and
    /// therefore this test, exists only where unix sockets do.
    #[cfg(unix)]
    #[test]
    fn the_socket_path_serves_the_same_lifecycle() {
        let dir = std::env::temp_dir().join(format!("arreo-mailbox-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let socket = dir.join("relay.sock");
        let mailbox = Arc::new(Mailbox::new());
        let server = Arc::clone(&mailbox);
        let path = socket.clone();
        std::thread::spawn(move || {
            let _ = serve_unix(&path, server);
        });
        // Wait for the listener.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !socket.exists() {
            assert!(Instant::now() < deadline, "the mailbox never bound");
            std::thread::sleep(Duration::from_millis(10));
        }

        // open → put → get → status → burn → refused, all over the socket.
        assert!(
            request_unix(
                &socket,
                &MailboxRequest::Open {
                    session: "s".into(),
                    ttl_secs: 30
                }
            )
            .expect("open")
            .ok
        );
        assert!(
            request_unix(
                &socket,
                &MailboxRequest::Put {
                    session: "s".into(),
                    slot: Slot::A,
                    payload: "flight".into(),
                },
            )
            .expect("put")
            .ok
        );
        let read = request_unix(
            &socket,
            &MailboxRequest::Get {
                session: "s".into(),
                slot: Slot::A,
            },
        )
        .expect("get");
        assert_eq!(read.payload.as_deref(), Some("flight"));
        // A slot is write-once over the wire too.
        assert!(
            !request_unix(
                &socket,
                &MailboxRequest::Put {
                    session: "s".into(),
                    slot: Slot::A,
                    payload: "forged".into(),
                },
            )
            .expect("put")
            .ok
        );
        let status = request_unix(
            &socket,
            &MailboxRequest::Status {
                session: "s".into(),
            },
        )
        .expect("status");
        assert!(status.ttl_ms.is_some_and(|ms| ms > 0));
        assert!(
            request_unix(
                &socket,
                &MailboxRequest::Burn {
                    session: "s".into()
                }
            )
            .expect("burn")
            .ok
        );
        let after = request_unix(
            &socket,
            &MailboxRequest::Status {
                session: "s".into(),
            },
        )
        .expect("status");
        assert!(!after.ok);
        // A malformed request is answered, not crashed on.
        let mut stream = UnixStream::connect(&socket).expect("connect");
        stream.write_all(b"{not json}\n").expect("write");
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply).expect("reply");
        let response: MailboxResponse = serde_json::from_str(&reply).expect("json reply");
        assert!(!response.ok);
        assert!(
            response
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("malformed"),
            "{response:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn frames_are_read_whole_and_oversized_ones_are_refused() {
        let mut reader = BufReader::new(&b"{\"op\":\"open\"}\n"[..]);
        assert_eq!(
            read_frame(&mut reader).expect("reads").as_deref(),
            Some("{\"op\":\"open\"}")
        );
        // A peer that closes cleanly without sending anything is "no request".
        let mut empty = BufReader::new(&b""[..]);
        assert_eq!(read_frame(&mut empty).expect("reads"), None);
        // A frame that never terminates is refused rather than buffered forever.
        let big = vec![b'x'; MAX_FRAME_BYTES + 8];
        let mut oversized = BufReader::new(big.as_slice());
        assert!(read_frame(&mut oversized).is_err());
        // A truncated frame (no newline before EOF) is an error, not a request.
        let mut truncated = BufReader::new(&b"{\"op\":\"ope"[..]);
        assert!(read_frame(&mut truncated).is_err());
    }
}
