//! T-0093, the daemon half: a background tick applies the operator's `[notify]`
//! policy to **every** agent state transition and records the outcome.
//!
//! The rule itself is `arreo_core::notify`, and its own tests cover the decision
//! order, the windows and the glob. What only a daemon can show is the wiring
//! around it, and that is what these tests assert — against a real socket and a
//! real audit log:
//!
//! 1. **A pane nobody is attached to is classified at all.** Every `pump` call
//!    site used to be client-driven (`Read`, `Wait`, `Send`, `PanesDetail`,
//!    `stream_attach`), so a pane with no client connected was never classified —
//!    and that is precisely the pane a notification exists for. The first test
//!    starts a real `arreo-server` process from a real configuration file,
//!    spawns a pane, **disconnects**, and then reads the notification out of the
//!    log. Nothing but the background tick can have written it.
//! 2. **Every transition gets a decision, and both answers are rows.** A
//!    withheld notification is a decision, not a silence: the states the policy
//!    does not care about are recorded as `notify.suppressed`, with the reason.
//! 3. **Off means off.** No `[notify]` section: not one row, because a daemon
//!    that never asked must not start appending to its audit log.
//! 4. **The row the daemon wrote is the history the rule reads back.** The
//!    episode rule asks what this pane was last *told*, and the daemon answers
//!    from its own log; this test drives that round trip from a row the daemon
//!    actually produced.
//!
//! The handoff case — the daemon that *takes over* resolving the policy for
//! itself, which is the shape T-0106 was — is
//! `the_notify_policy_reaches_the_daemon_that_takes_over` in `tests/handoff.rs`,
//! beside the rest of that file's cut machinery.
//!
//! Scratch lives under `target/test-scratch/` (never `/tmp`: it is a tmpfs here,
//! and a pane's pty plus a SQLite log under it is a real problem).

use arreo_core::notify::{self, Decision, History, Policy, SuppressReason, Transition};
use arreo_core::proto::codec;
use arreo_core::proto::{AgentState, Message, VERSION};
use arreo_core::store::{actions, AuditOutcome, SessionStore, StoredAudit};
use arreo_server::daemon::Daemon;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// How long a test waits for the background tick to write a row. Generous: the
/// tick is a 1 s loop and a pane only *becomes* `question` after the adapter's
/// 2 s of quiet, so the row cannot exist immediately — and a loaded machine is
/// slow. A failure prints what the log did hold.
const WAIT: Duration = Duration::from_secs(20);

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/test-scratch/T-0093/integration")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    std::fs::canonicalize(&dir).expect("canonical")
}

/// A configuration file written the way an operator writes one.
fn config_file(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("arreo.toml");
    std::fs::write(&path, body).expect("write the configuration file");
    path
}

/// The daemon's own audit store for a socket — from the one function that
/// decides that path, so this test reads the log the daemon writes.
fn store_path(socket: &Path) -> PathBuf {
    arreo_server::db_path_for(socket)
}

/// Every notification row under `action`, newest first — the whole log's answer,
/// not one pane's, so a test can assert that *nothing* was written.
fn rows_all(db: &Path, action: &str) -> Vec<StoredAudit> {
    let store = SessionStore::open(db).expect("open the daemon's store");
    store.audit_by_action(action, 100).expect("read the log")
}

async fn wait_for_rows<F>(db: &Path, action: &str, pane: &str, mut ok: F) -> Vec<StoredAudit>
where
    F: FnMut(&[StoredAudit]) -> bool,
{
    let deadline = Instant::now() + WAIT;
    loop {
        let found: Vec<StoredAudit> = rows_all(db, action)
            .into_iter()
            .filter(|row| row.agent == pane)
            .collect();
        if ok(&found) {
            return found;
        }
        assert!(
            Instant::now() < deadline,
            "no {action} row for {pane} within {}s; the log held: {found:?}",
            WAIT.as_secs()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Framed MessagePack client: Hello handshake on connect, then request loop.
struct Client {
    stream: tokio::net::unix::OwnedWriteHalf,
    reader: tokio::net::unix::OwnedReadHalf,
    buf: Vec<u8>,
}

impl Client {
    async fn connect(socket: &Path) -> Self {
        let stream = tokio::net::UnixStream::connect(socket)
            .await
            .expect("connect");
        let (reader, stream) = stream.into_split();
        let mut client = Self {
            stream,
            reader,
            buf: Vec::new(),
        };
        client
            .send(&Message::Hello {
                v: VERSION,
                client: "notify-test".to_string(),
                wants: vec![VERSION],
            })
            .await;
        match client.recv().await {
            Message::Welcome { v, .. } => assert_eq!(v, VERSION),
            other => panic!("want Welcome, got {other:?}"),
        }
        client
    }

    async fn send(&mut self, message: &Message) {
        let frame = codec::encode_frame(message).expect("encode");
        self.stream.write_all(&frame).await.expect("write");
        self.stream.flush().await.expect("flush");
    }

    async fn recv(&mut self) -> Message {
        loop {
            if let Ok((message, consumed)) = codec::decode_frame(&self.buf) {
                self.buf.drain(..consumed);
                return message;
            }
            let mut chunk = [0u8; 8192];
            let n = tokio::time::timeout(Duration::from_secs(10), self.reader.read(&mut chunk))
                .await
                .expect("read timeout")
                .expect("read");
            assert!(n > 0, "server closed connection");
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }

    async fn call(&mut self, message: &Message) -> Message {
        self.send(message).await;
        self.recv().await
    }
}

/// Spawn the pane every test in this file uses: it asks a question and then
/// waits for an answer.
///
/// `Proceed? [y/n]` matches the universal adapter's `proceed\?`/`\[y/n\]`
/// question patterns, and `sleep 30` leaves the tail unchanged and quiet — so
/// the engine infers `question` from silence and a prompt-shaped tail without
/// anybody typing. The shell never prints a prompt of its own (`-c`, not `-i`),
/// so nothing competes with the question.
async fn spawn_asking_pane(client: &mut Client, id: &str) {
    let reply = client
        .call(&Message::Spawn {
            v: VERSION,
            id: id.to_string(),
            program: "/bin/sh".to_string(),
            args: vec![
                "-c".to_string(),
                "printf 'Proceed? [y/n]\\n'; sleep 30".to_string(),
            ],
            cols: 80,
            rows: 24,
            memory_max: None,
            pids_max: None,
            kill_on_breach: false,
        })
        .await;
    assert!(matches!(reply, Message::Ok { .. }), "spawn: {reply:?}");
}

/// An in-process daemon on its own socket, with `policy`, and one asking pane —
/// attached only long enough to spawn it.
///
/// Dropping the client is the point: from that moment nobody is attached, and
/// the only thing that can classify the pane is the background tick. Returns the
/// store path to read the outcome from.
async fn daemon_with_asking_pane(dir: &Path, policy: Option<Policy>, id: &str) -> PathBuf {
    let socket = dir.join("arreo.sock");
    let daemon = Daemon::new(&socket).with_notify_policy(policy);
    tokio::spawn(async move {
        let _ = daemon.serve().await;
    });
    // The bind happens inside `serve`; connecting before it would race.
    let deadline = Instant::now() + Duration::from_secs(10);
    while tokio::net::UnixStream::connect(&socket).await.is_err() {
        assert!(
            Instant::now() < deadline,
            "the daemon never bound {}",
            socket.display()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let mut client = Client::connect(&socket).await;
    spawn_asking_pane(&mut client, id).await;
    drop(client);
    store_path(&socket)
}

// ---------------------------------------------------------------------------
// 1. Nobody attached, and the operator is still told
// ---------------------------------------------------------------------------

/// A real `arreo-server` **process**, started the way an operator starts it —
/// `--socket`, `--config`, its own identity directory — so the whole chain runs:
/// the configuration file, `main`'s resolution of it, the daemon, the tick, the
/// row.
struct ServerChild {
    child: std::process::Child,
}

impl ServerChild {
    fn start(socket: &Path, config: &Path, dir: &Path) -> Self {
        std::fs::create_dir_all(dir.join("identity")).expect("identity dir");
        let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_arreo-server"))
            .arg("--socket")
            .arg(socket)
            .arg("--config")
            .arg(config)
            .env("ARREO_IDENTITY_DIR", dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("arreo-server starts");
        // Bounded wait for the socket: a daemon that exits instead is a failure
        // with the child's own log attached.
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if std::os::unix::net::UnixStream::connect(socket).is_ok() {
                return Self { child };
            }
            if let Ok(Some(status)) = child.try_wait() {
                let mut log = String::new();
                if let Some(stderr) = child.stderr.take() {
                    use std::io::Read;
                    let _ = std::io::BufReader::new(stderr).read_to_string(&mut log);
                }
                panic!("the daemon exited before serving ({status}): {log}");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
        panic!("the daemon never bound {}", socket.display());
    }
}

impl Drop for ServerChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[tokio::test]
async fn a_configured_daemon_notifies_with_nobody_attached() {
    let dir = scratch("nobody-attached");
    let config = config_file(
        &dir,
        // The operator's opt-in, and nothing else: a `[notify]` section with the
        // default behaviour (the two states that need a human, once per episode).
        "[notify]\n",
    );
    let socket = dir.join("arreo.sock");
    let _server = ServerChild::start(&socket, &config, &dir);

    let pane = "notify-detached";
    let mut client = Client::connect(&socket).await;
    spawn_asking_pane(&mut client, pane).await;
    // **Nobody is attached from here on.** No `Read`, no `Wait`, no `PanesDetail`
    // — so no client-driven `pump` can have classified this pane; whatever the
    // log holds was produced by the background tick.
    drop(client);

    let db = store_path(&socket);
    let sent = wait_for_rows(&db, actions::NOTIFY_SENT, pane, |rows| !rows.is_empty()).await;
    let row = &sent[0];
    assert_eq!(row.device, "daemon", "the tick is the actor, not a session");
    assert_eq!(row.outcome, AuditOutcome::Ok);
    // `state=<word>; <sentence>` — the format written and read by the library's
    // own `detail_for`/`state_from_detail` pair, not hand-rolled on either side.
    let detail = row.detail.as_deref().unwrap_or_default();
    assert_eq!(
        notify::state_from_detail(detail),
        Some(AgentState::Question),
        "the row is about the question: {detail:?}"
    );
    assert!(
        detail.starts_with("state=question;"),
        "the sentence follows the state prefix: {detail:?}"
    );
    assert!(
        row.prompt.contains("question"),
        "the prompt is the human sentence: {:?}",
        row.prompt
    );
}

// ---------------------------------------------------------------------------
// 2. Every transition gets a decision — including "no"
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_withheld_transition_is_recorded_too() {
    let dir = scratch("withheld");
    let policy = Policy::load(&config_file(&dir, "[notify]\n"))
        .expect("the section parses")
        .expect("the section is present");
    let db = daemon_with_asking_pane(&dir, Some(policy), "notify-withheld").await;

    // The pane printing is a transition to `working`, which the policy has no
    // rule for. It must not vanish: the operator's "why was I not told?" has to
    // have an answer for the transitions that were *considered*, not only for
    // the ones that were delivered.
    let suppressed = wait_for_rows(&db, actions::NOTIFY_SUPPRESSED, "notify-withheld", |rows| {
        rows.iter().any(|row| {
            row.detail
                .as_deref()
                .unwrap_or_default()
                .contains("no-rule")
        })
    })
    .await;
    let row = suppressed
        .iter()
        .find(|row| {
            row.detail
                .as_deref()
                .unwrap_or_default()
                .contains("no-rule")
        })
        .expect("the no-rule row");
    assert_eq!(
        row.outcome,
        AuditOutcome::Refused,
        "a decision not to act is a refusal, not a success"
    );
    assert_eq!(
        notify::state_from_detail(row.detail.as_deref().unwrap_or_default()),
        Some(AgentState::Working),
        "the row is about the working transition: {:?}",
        row.detail
    );

    // And the same run did deliver the question: both answers, one log.
    wait_for_rows(&db, actions::NOTIFY_SENT, "notify-withheld", |rows| {
        !rows.is_empty()
    })
    .await;
}

// ---------------------------------------------------------------------------
// 3. Off means off
// ---------------------------------------------------------------------------

#[tokio::test]
async fn no_notify_section_writes_no_rows_at_all() {
    let dir = scratch("off");
    // No `[notify]` anywhere in the file: notifications are off, which is the
    // default and the answer for every daemon that never asked.
    let config = config_file(&dir, "# nothing to notify about\n");
    assert!(
        Policy::load(&config)
            .expect("a file without the section is not an error")
            .is_none(),
        "no section is not a policy"
    );
    let db = daemon_with_asking_pane(&dir, None, "notify-off").await;

    // Long enough for the pane to have printed, gone quiet, and been classified
    // as `question` — the transition a configured daemon would have notified on.
    tokio::time::sleep(Duration::from_secs(6)).await;
    for action in [actions::NOTIFY_SENT, actions::NOTIFY_SUPPRESSED] {
        let all = rows_all(&db, action);
        assert!(
            all.is_empty(),
            "a daemon with no [notify] section wrote {action} rows: {all:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 4. The row the daemon writes is the history the rule reads
// ---------------------------------------------------------------------------

/// The episode rule (`once_per_episode`) turns on `from == to`, and the daemon
/// reconstructs `last_notified_state` from its own `notify.sent` rows.
///
/// **`from == to` cannot reach the daemon.** Not because every push is guarded —
/// the bell push is not (`engine.rs`, the `bell_means_attention` arm assigns and
/// pushes unconditionally) — but because of what that arm's input necessarily is:
/// a BEL is itself output, so the same feed has already taken the pane to
/// `working`, and the bell's own event therefore *changes* state. Every other
/// push (`working`, and each `tick` arm) is explicitly guarded by `state != X`.
/// The net effect is that **consecutive events always differ**, chain them or not,
/// so a `Transition` with `from == to` is not producible by driving a pane — and a
/// daemon-level test claiming to exercise `same-episode` end to end would be
/// asserting something the engine cannot do. (Probed directly: two BELs produce
/// `working` then `blocked`, never `blocked` then `blocked`.)
///
/// The half that *is* the daemon's is the round trip, and that is what this test
/// drives: the row this daemon wrote, read back through the library's own reader,
/// is exactly the history that makes the rule answer `same-episode`.
#[tokio::test]
async fn the_written_row_is_the_history_the_episode_rule_reads() {
    let dir = scratch("history");
    let policy = Policy::load(&config_file(&dir, "[notify]\n"))
        .expect("the section parses")
        .expect("the section is present");
    let pane = "notify-history";
    let db = daemon_with_asking_pane(&dir, Some(policy.clone()), pane).await;

    let rows = wait_for_rows(&db, actions::NOTIFY_SENT, pane, |rows| !rows.is_empty()).await;
    // Newest first: the row the daemon wrote for the question.
    let newest = &rows[0];

    // Reconstruct the history exactly as the daemon's own read does.
    let history = History {
        last_notified_ms: Some(newest.ts_ms),
        last_notified_state: newest.detail.as_deref().and_then(notify::state_from_detail),
    };
    assert_eq!(history.last_notified_state, Some(AgentState::Question));

    let again = Transition {
        pane: pane.to_string(),
        machine: arreo_core::mesh::default_machine_name(),
        at_ms: newest.ts_ms + 1,
        from: AgentState::Question,
        to: AgentState::Question,
        reason: newest.prompt.clone(),
    };
    match policy.decide(&again, &history) {
        Decision::Suppressed {
            reason: SuppressReason::SameEpisode { state },
        } => assert_eq!(state, AgentState::Question),
        other => panic!("want a same-episode suppression, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 5. A cycle that comes back, and the coalescing window
// ---------------------------------------------------------------------------

/// The script a pane that asks **twice** runs: a question, a pause, the same
/// question again.
///
/// That is the one sequence which exercises the criteria's own sentence — "a
/// blocked→working→blocked cycle notifies twice" — end to end, because the second
/// prompt is new output (so the pane leaves `question`), and `question_after_ms`
/// of quiet puts it back. The 5 s gap is longer than the adapter's 2 s
/// `question_after_ms` (so the first is classified before the second arrives) and
/// far shorter than any coalescing window a test configures (so the second lands
/// inside it) — which is what makes the pair of tests below a controlled
/// comparison: the same sequence, one knob apart.
const ASKS_TWICE: &str =
    "printf 'Proceed? [y/n]\\n'; sleep 5; printf 'Proceed? [y/n]\\n'; sleep 60";

/// The detached-daemon shape of [`daemon_with_asking_pane`], for a pane whose
/// script is the test's business. Same contract: the client is dropped, so only
/// the background tick can classify the pane.
async fn daemon_with_script(dir: &Path, policy: Option<Policy>, id: &str, script: &str) -> PathBuf {
    let socket = dir.join("arreo.sock");
    let daemon = Daemon::new(&socket).with_notify_policy(policy);
    tokio::spawn(async move {
        let _ = daemon.serve().await;
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while tokio::net::UnixStream::connect(&socket).await.is_err() {
        assert!(
            Instant::now() < deadline,
            "the daemon never bound {}",
            socket.display()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let mut client = Client::connect(&socket).await;
    let reply = client
        .call(&Message::Spawn {
            v: VERSION,
            id: id.to_string(),
            program: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), script.to_string()],
            cols: 80,
            rows: 24,
            memory_max: None,
            pids_max: None,
            kill_on_breach: false,
        })
        .await;
    assert!(matches!(reply, Message::Ok { .. }), "spawn: {reply:?}");
    drop(client);
    store_path(&socket)
}

/// **A cycle that returns to the state notifies twice** — the criterion's first
/// half, observed end to end with coalescing switched off.
///
/// The alternative design (keying the episode rule on "the last state I told you
/// about" rather than on the state *changing away and back*) would swallow this:
/// the operator would answer one question and never learn there was a second.
/// That is why the rule is written the way it is, and why this test exists.
#[tokio::test]
async fn a_cycle_that_comes_back_notifies_twice() {
    let dir = scratch("cycle");
    let policy = Policy::load(&config_file(&dir, "[notify]\ncoalesce_secs = 0\n"))
        .expect("the section parses")
        .expect("the section is present");
    let pane = "notify-cycle";
    let db = daemon_with_script(&dir, Some(policy), pane, ASKS_TWICE).await;

    let rows = wait_for_rows(&db, actions::NOTIFY_SENT, pane, |rows| rows.len() >= 2).await;
    assert!(
        rows.len() >= 2,
        "the second question is news and must be delivered: {rows:?}"
    );
    // Both are about the question, and they are two distinct moments — not one
    // row read twice.
    assert!(rows.iter().take(2).all(|row| row
        .detail
        .as_deref()
        .and_then(notify::state_from_detail)
        .is_some_and(|state| state == AgentState::Question)));
    assert_ne!(
        rows[0].ts_ms, rows[1].ts_ms,
        "two deliveries, two moments: {rows:?}"
    );
}

/// **The same sequence, one knob apart: the second is held by the coalescing
/// window** — and the holding is *recorded*, which is the whole point ("why was I
/// not told?").
#[tokio::test]
async fn a_second_transition_inside_the_window_is_coalesced_and_counted() {
    let dir = scratch("coalesce");
    let policy = Policy::load(&config_file(&dir, "[notify]\ncoalesce_secs = 300\n"))
        .expect("the section parses")
        .expect("the section is present");
    let pane = "notify-coalesce";
    let db = daemon_with_script(&dir, Some(policy), pane, ASKS_TWICE).await;

    // The suppression is what we are waiting for: it can only exist once the
    // second transition has been decided, which is after the second prompt.
    let held = wait_for_rows(&db, actions::NOTIFY_SUPPRESSED, pane, |rows| {
        rows.iter().any(|row| {
            row.detail
                .as_deref()
                .is_some_and(|d| d.contains("coalesced"))
        })
    })
    .await;
    let coalesced = held
        .iter()
        .find(|row| {
            row.detail
                .as_deref()
                .is_some_and(|d| d.contains("coalesced"))
        })
        .expect("a coalesced row");
    assert_eq!(
        coalesced.outcome,
        AuditOutcome::Refused,
        "a suppression is a decision not to act"
    );
    assert_eq!(
        notify::state_from_detail(coalesced.detail.as_deref().unwrap_or("")),
        Some(AgentState::Question)
    );

    // And exactly one delivery went out for the pane in that window.
    let sent = rows_all(&db, actions::NOTIFY_SENT)
        .into_iter()
        .filter(|row| row.agent == pane)
        .count();
    assert_eq!(sent, 1, "one notification per pane per window");
}

// ---------------------------------------------------------------------------
// 6. A transition a client's poll consumed is still decided
// ---------------------------------------------------------------------------

/// **A transition is decided even when a client's poll is what consumed it.**
///
/// The engine fires each state change exactly once, and every client-side pump
/// drops the events it does not want. So before the tick compared the state it
/// had last seen against the state now, a transition that a poll consumed reached
/// **no decision at all** — not even the "no rule" row every other transition
/// gets. That is the common case, not a corner: the TUI asks for the whole wall
/// (`PanesDetail`) once per pass, so it races the 1 s tick for every transition on
/// every pane (found by review, reproduced against the running daemon).
///
/// This test is that race, deliberately: it polls `PanesDetail` in a tight loop
/// for the whole time the pane is becoming `question`, so the transition is
/// consumed by the client rather than by the tick. The row must exist anyway.
#[tokio::test]
async fn a_transition_a_poll_consumed_is_still_decided() {
    let dir = scratch("poll-consumed");
    // The default policy, so the question is one the operator asked about.
    let policy = Policy::load(&config_file(&dir, "[notify]\n"))
        .expect("the section parses")
        .expect("the section is present");
    let pane = "notify-polled";

    let socket = dir.join("arreo.sock");
    let daemon = Daemon::new(&socket).with_notify_policy(Some(policy));
    tokio::spawn(async move {
        let _ = daemon.serve().await;
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while tokio::net::UnixStream::connect(&socket).await.is_err() {
        assert!(Instant::now() < deadline, "the daemon never bound");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let mut client = Client::connect(&socket).await;
    spawn_asking_pane(&mut client, pane).await;

    // **Poll the way the TUI does**, for long enough to cover the transition: the
    // pane needs 2 s of quiet to become `question`, and the poll runs throughout,
    // so the client's pump is the one that classifies it.
    let poll_until = Instant::now() + Duration::from_secs(8);
    let mut polls = 0usize;
    while Instant::now() < poll_until {
        let reply = client
            .call(&Message::PanesDetail {
                v: VERSION,
                panes: Vec::new(),
            })
            .await;
        assert!(
            matches!(reply, Message::PanesDetail { .. }),
            "the poll must succeed: {reply:?}"
        );
        polls += 1;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(polls > 20, "the poll loop ran: {polls} polls");
    drop(client);

    // The decision exists despite the tick never being the one to see the change.
    let db = store_path(&socket);
    let rows = wait_for_rows(&db, actions::NOTIFY_SENT, pane, |rows| !rows.is_empty()).await;
    assert!(
        rows.iter().any(
            |row| notify::state_from_detail(row.detail.as_deref().unwrap_or(""))
                == Some(AgentState::Question)
        ),
        "the question was decided even though a poll consumed it: {rows:?}; \
         every notify row in the log: {:?}",
        rows_all(&db, actions::NOTIFY_SENT)
    );
}

// ---------------------------------------------------------------------------
// 7. The provenance outlives the tick that consumed the transition
// ---------------------------------------------------------------------------

/// **`wait` answers with how the state was derived, whoever observed the
/// transition** — the client-visible half of the ungated tick (T-0110).
///
/// The tick pumps every pane once a second and is deliberately not gated on a
/// `[notify]` section (that is what makes it classify an unattached pane), so the
/// transition a waiting client's own pump used to produce is normally *already
/// consumed* by the time the client asks. `Wait` then takes its "already in the
/// wanted state" branch — which synthesised `direct:already` and dropped the
/// matched pattern. The state was right; the provenance was gone, and for a
/// `question` the pattern is the only field that says *what* the pane is asking.
///
/// This is that sequence, driven the way the defect was reported: a real
/// `arreo-server` process, a real configuration file with **no `[notify]`
/// section at all** (the default, i.e. every existing user), a pane that asks,
/// and the tick left to classify it with nobody attached. Then a client connects
/// and asks. The reply must carry the engine's own derivation and the pattern it
/// matched — the same two fields the event path fills.
#[tokio::test]
async fn wait_reports_the_provenance_the_tick_consumed() {
    let dir = scratch("wait-provenance");
    // No `[notify]` anywhere in the file. The tick pumps regardless (a daemon
    // that classified its panes only when a client asked is the bug the tick
    // closes); notifications are simply off, so the log stays empty.
    let config = config_file(&dir, "# nothing to notify about\n");
    assert!(
        Policy::load(&config)
            .expect("a file without the section is not an error")
            .is_none(),
        "no section is not a policy"
    );
    let socket = dir.join("arreo.sock");
    let _server = ServerChild::start(&socket, &config, &dir);

    let pane = "wait-provenance";
    let mut spawner = Client::connect(&socket).await;
    spawn_asking_pane(&mut spawner, pane).await;
    // **Nobody is attached from here on**, and the pane is left alone long enough
    // for the tick to classify it: the prompt lands immediately, `question` needs
    // the adapter's 2 s of quiet, and the tick is a 1 s loop — so ~4 s is the
    // earliest and 5 s leaves a slow machine room. By the time the client below
    // asks, the transition exists only in the engine's memory of it.
    drop(spawner);
    tokio::time::sleep(Duration::from_secs(5)).await;

    let mut client = Client::connect(&socket).await;
    let reply = client
        .call(&Message::Wait {
            v: VERSION,
            id: pane.to_string(),
            state: AgentState::Question,
            timeout_ms: 5_000,
        })
        .await;
    match reply {
        Message::StateEvent {
            state,
            confidence,
            matched_pattern,
            ..
        } => {
            assert_eq!(state, AgentState::Question);
            assert_eq!(
                confidence, "inferred:silence+prompt-shape",
                "the engine's own derivation — `direct:already` claims the pane \
                 was already in the state before the watch, which is the lie"
            );
            assert_eq!(
                matched_pattern.as_deref(),
                Some("\\[y/n\\]"),
                "the pattern that fired is what says what the pane is asking"
            );
        }
        other => panic!("want the question event, got {other:?}"),
    }
}

/// **`direct:already` survives only where it is true** — the other half of the
/// distinction (T-0110).
///
/// A pane that has printed nothing has never transitioned: the engine's state is
/// the initial `Unknown`, derived from no observation at all. That is the one
/// answer `direct:already` was ever right for, and filling the reply from the
/// engine's last transition must not swallow it — a pane nobody has classified
/// must not be reported as classified. (`arreo wait <pane> --state unknown` is a
/// real CLI request, so this is reachable, not hypothetical.)
#[tokio::test]
async fn wait_still_says_already_when_no_transition_derived_the_state() {
    let dir = scratch("wait-already");
    let config = config_file(&dir, "# nothing to notify about\n");
    let socket = dir.join("arreo.sock");
    let _server = ServerChild::start(&socket, &config, &dir);

    // A pane that prints nothing, so the engine never sees output and never
    // transitions.
    let pane = "wait-already";
    let mut client = Client::connect(&socket).await;
    let reply = client
        .call(&Message::Spawn {
            v: VERSION,
            id: pane.to_string(),
            program: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), "sleep 60".to_string()],
            cols: 80,
            rows: 24,
            memory_max: None,
            pids_max: None,
            kill_on_breach: false,
        })
        .await;
    assert!(matches!(reply, Message::Ok { .. }), "spawn: {reply:?}");

    let reply = client
        .call(&Message::Wait {
            v: VERSION,
            id: pane.to_string(),
            state: AgentState::Unknown,
            timeout_ms: 5_000,
        })
        .await;
    match reply {
        Message::StateEvent {
            state,
            confidence,
            matched_pattern,
            ..
        } => {
            assert_eq!(state, AgentState::Unknown);
            assert_eq!(
                confidence, "direct:already",
                "no transition has derived this state, so there is no provenance to quote"
            );
            assert_eq!(matched_pattern, None);
        }
        other => panic!("want the unknown event, got {other:?}"),
    }
}
