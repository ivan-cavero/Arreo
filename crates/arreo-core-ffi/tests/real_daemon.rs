#![cfg(unix)]

//! T-0125: the real-daemon retest, durable — the exported surface against real
//! processes.
//!
//! T-0114 shipped a **green, wrong** tree: 936 tests, 14/14 slices, bench 6/6,
//! vet/deny/audit all passing, and the feature was one-shot against a real
//! daemon. The battery could not see it because every check it runs is either a
//! unit test or a fixture, and **the fixture was not a daemon**: it closed its
//! session after each answer, which forced the reconnect path and hid a failure
//! any real machine produces on the second read. The proof that found it was a
//! scratch probe under `target/` (gitignored, so the proof would evaporate with
//! the build directory). This is that probe made durable.
//!
//! What it drives, and through which door:
//!
//! - the relay and the machine's daemon are the product's own binaries, spawned
//!   as processes (`arreo-relay serve`, `arreo-server`), **never linked** — the
//!   relay is AGPL and the server is the product, so this file must not be able
//!   to call either one;
//! - the machine's identity files are written the way `arreo pair` leaves them,
//!   and its certificate is issued through the **boundary**
//!   (`device_cert_issue`);
//! - the machine pins both phones through the **real CLI** (`arreo devices
//!   issue`), which is the one step a deployment does by pairing;
//! - the phone is the exported surface: `relay_session_dial`, `machines`, then
//!   `metrics_history` three times on one session, an act, a wrong-but-valid
//!   pinned key, and an empty window.
//!
//! The two reads are the point. A real daemon answers the first and keeps its
//! session open; a client that handshakes per call fails the second (revert the
//! conversation cache and this file prints `read #2 FAIL — the SECOND read
//! failed: handshake took longer than 10s`, which is T-0114's M1' on real
//! processes).
//!
//! **Why `#[ignore]`.** The check is a gate, not a unit test: it needs three
//! built binaries, a QUIC loopback listener and ~40 s of real sampling, and it
//! must be able to answer SKIP rather than FAIL when the machine cannot run it.
//! `cargo xtask e2e --slice ffi` is what runs it (and is the only thing that
//! turns its evidence into a PASS); `cargo test --workspace` deliberately does
//! not, so the unit suite stays a unit suite.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use arreo_core_ffi::codec::WireNotifyAction;
use arreo_core_ffi::errors::SessionFfiError;
use arreo_core_ffi::identity::{
    device_cert_issue, device_key_from_seed, fingerprint_of_public_key, role_word,
    root_key_from_seed, DeviceCertHandle, DeviceKeyHandle, FfiRole,
};
use arreo_core_ffi::relay::{
    relay_peer_parse, relay_session_dial, MetricsSeriesInfo, RelayPeerHandle, RelaySessionHandle,
};

/// The prefix every line this file prints carries: the slice reads the terminal
/// summary out of the test output, so the evidence has one spelling.
const SLICE: &str = "ffi-slice";

const ACCOUNT: &str = "acct-t0125";
/// The account's root key: registers the account with the relay and issues both
/// machines' certificates. Also the machine's *directory* root, as `arreo pair`
/// leaves it.
const ACCOUNT_SEED: [u8; 32] = [0x11; 32];
/// The machine's device key — what its daemon authenticates with.
const MACHINE_SEED: [u8; 32] = [0x44; 32];
/// The phone that reads: admitted as a **viewer**, which is what a phone is and
/// which holds the `Observe` capability a metrics read needs.
const PHONE_SEED: [u8; 32] = [0x22; 32];
/// The phone that answers: an act needs `Capability::Control`, which only the
/// owner role holds (`identity::role::required`), so the act door cannot be
/// covered by the viewer's session.
const OWNER_SEED: [u8; 32] = [0x55; 32];
/// Some other device's key, for the wrong-but-valid pin.
const STRANGER_SEED: [u8; 32] = [0x33; 32];
const MACHINE_NAME: &str = "workbox";
const PHONE_NAME: &str = "pixel-7";
const OWNER_NAME: &str = "desk-phone";
/// The pane the meters read: a long-lived child, sampled by the daemon's own
/// metrics writer.
const PANE: &str = "pane-a";
/// A pane spawned at the moment of the empty-window read: no history yet.
const FRESH: &str = "pane-b";
/// The pane the owner answers: it prints a prompt the universal adapter reads as
/// a question and then blocks on `read`, exactly like `arreo-server`'s own
/// notify tests spawn.
const ASKING: &str = "pane-q";
const ASKING_SCRIPT: &str = "printf 'Proceed? [y/n]\\n'; read ans; echo \"ANSWER=$ans\"; sleep 300";
const IDLE_SCRIPT: &str = "sleep 300";

/// The tier the machine's metrics writer records at (T-0040), and therefore the
/// tier a read over a short window is served.
const TIER_MS: u64 = 10_000;
/// The three tiers the store keeps (`metrics_retention`'s steps). A read that
/// named anything else would be the *ask* echoed back rather than the tier the
/// machine served, which is the distinction `step_ms` exists to carry.
const REAL_TIERS: [u64; 3] = [10_000, 60_000, 3_600_000];

/// The machine cannot run this check here — as opposed to the product failing
/// it. The two are kept apart on purpose: `check-targets` established the shape
/// (name what is missing, never fake a PASS), and a slice that reddens because
/// a binary was not built teaches nobody anything.
enum Failure {
    Skip(String),
    Check(String),
}

impl Failure {
    fn skip(message: impl Into<String>) -> Self {
        Self::Skip(message.into())
    }

    fn check(message: impl Into<String>) -> Self {
        Self::Check(message.into())
    }
}

type Outcome<T> = std::result::Result<T, Failure>;

/// What the slice reports, and what the runner re-checks before it will call a
/// run a PASS: a PASS without real rows would be the green-and-wrong tree again.
#[derive(Debug)]
struct Summary {
    reads: usize,
    rows_min: usize,
    tier_ms: u64,
    empty_rows: usize,
    acts: usize,
    kills: usize,
}

impl Summary {
    fn new() -> Self {
        Self {
            reads: 0,
            rows_min: usize::MAX,
            tier_ms: 0,
            empty_rows: usize::MAX,
            acts: 0,
            kills: 0,
        }
    }

    fn took(&mut self, series: &MetricsSeriesInfo) {
        self.reads += 1;
        self.rows_min = self.rows_min.min(series.rows.len());
        self.tier_ms = series.step_ms;
    }

    fn line(&self) -> String {
        format!(
            "reads={} rows_min={} tier_ms={} empty_rows={} acts={} kills={}",
            self.reads, self.rows_min, self.tier_ms, self.empty_rows, self.acts, self.kills
        )
    }
}

#[test]
#[ignore = "spawns the real binaries; `cargo xtask e2e --slice ffi` runs it"]
fn two_reads_and_an_act_against_a_real_daemon() {
    match run() {
        Ok(summary) => println!("{SLICE}: PASS {}", summary.line()),
        Err(Failure::Skip(reason)) => println!("{SLICE}: SKIP ({reason})"),
        Err(Failure::Check(message)) => panic!("{SLICE}: {message}"),
    }
}

fn run() -> Outcome<Summary> {
    let mut world = World::start()?;
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|e| Failure::check(format!("no tokio runtime: {e}")))?;
    let outcome = runtime.block_on(drive(&world));
    // The relay and the daemon go whether the drive passed or failed: a failing
    // run must not leave two processes behind for the next one to trip over.
    world.shutdown();
    outcome
}

/// The whole check, in the order the doors are opened.
async fn drive(world: &World) -> Outcome<Summary> {
    let mut summary = Summary::new();

    // ---- the phone, as a viewer: the door T-0114 fixed ----
    let viewer = world.pin(PHONE_SEED, PHONE_NAME, FfiRole::Viewer, 2)?;
    let session = dial(world, &viewer, "viewer").await?;
    let (peer, daemon_key) = world.await_row(&session, "viewer").await?;

    // The pane is sampled every 10 s by the machine's metrics writer; give it an
    // interval and a half so the first read has a row to return.
    say(&format!(
        "machine up; waiting for the metrics writer (a {TIER_MS} ms tier)"
    ));
    std::thread::sleep(Duration::from_secs(15));
    let now = now_ms();

    let first = read(
        &session,
        &peer,
        &daemon_key,
        PANE,
        (now.saturating_sub(300_000), u64::MAX),
        0,
        ("read #1", "the FIRST read failed"),
    )
    .await?;
    served(&first, "read #1")?;
    summary.took(&first);
    say(&format!(
        "read #1 ok step_ms={} downshifted={} rows={}",
        first.step_ms,
        first.downshifted,
        first.rows.len()
    ));

    // **The p1.** A real daemon is still holding the conversation the first read
    // opened; a client that handshakes per call fails here — and only here,
    // which is why the two reads are the check rather than one.
    let second = read(
        &session,
        &peer,
        &daemon_key,
        PANE,
        (now.saturating_sub(60_000), u64::MAX),
        1_000,
        ("read #2", "the SECOND read failed"),
    )
    .await?;
    served(&second, "read #2")?;
    summary.took(&second);
    say(&format!(
        "read #2 ok step_ms={} downshifted={} rows={} (the conversation was reused)",
        second.step_ms,
        second.downshifted,
        second.rows.len()
    ));

    // ---- the act door (T-0115) on that same conversation ----
    //
    // A viewer's act is refused by the machine's own gate — `skip` and `reply`
    // are `Verb::Send` and need `Capability::Control` — so what this proves is
    // that the verb *reached* the machine on the conversation the reads opened:
    // a client that handshakes per call would fail here with a channel error,
    // not with the gate's sentence. The expected sentence is built from the
    // core's own rules (`VerbDenial::Role` wrapping `role::check`) rather than
    // typed, so the two cannot drift.
    let denial = denial_for_a_viewer_sending(&viewer.id)?;
    match session
        .notify_act(
            peer.clone(),
            daemon_key.clone(),
            PANE.to_string(),
            WireNotifyAction::Skip,
            None,
        )
        .await
    {
        Ok(()) => {
            return Err(Failure::check(
                "act FAIL — a viewer's skip was taken: the machine's gate admitted a viewer",
            ))
        }
        Err(SessionFfiError::Daemon(sentence)) if sentence == denial => {
            summary.acts += 1;
            say(&format!("viewer act refused: {sentence}"));
        }
        Err(SessionFfiError::Daemon(sentence)) => {
            return Err(Failure::check(format!(
                "act FAIL — refused with {sentence:?}, not the gate's sentence {denial:?}"
            )))
        }
        Err(other) => {
            return Err(Failure::check(format!(
                "act FAIL — the act never reached the machine: {other:?}"
            )))
        }
    }

    // The refusal was an *answer*, so the conversation is still the one the first
    // read opened. This read is load-bearing: a refusal that dropped the
    // conversation would leave the daemon holding the old session, and the next
    // read would open a second one into it — the p1 again.
    let third = read(
        &session,
        &peer,
        &daemon_key,
        PANE,
        (now.saturating_sub(60_000), u64::MAX),
        0,
        ("read #3", "the read after the refused act failed"),
    )
    .await?;
    served(&third, "read #3")?;
    summary.took(&third);
    say(&format!(
        "read #3 ok rows={} (a refusal is an answer: the conversation survived it)",
        third.rows.len()
    ));

    // ---- a wrong-but-valid pinned key ----
    //
    // The pin is what a phone stores at pairing, so this is the one check that
    // says the relay's directory cannot talk a phone into reading a machine under
    // a key it never pinned. The assertion is on the **positive symptom**: the
    // pane's data must not come back. (T-0114's M2 is the cautionary tale — an
    // assertion that only expected `Peer(_)` stayed green under a mutation that
    // served the read under the wrong key.)
    let stranger = device_key_from_seed(STRANGER_SEED.to_vec())
        .map_err(|e| Failure::check(format!("wrong key FAIL — a key from a seed: {e}")))?;
    match session
        .metrics_history(
            peer.clone(),
            stranger.public_hex(),
            PANE.to_string(),
            now.saturating_sub(60_000),
            u64::MAX,
            0,
        )
        .await
    {
        Ok(series) => {
            return Err(Failure::check(format!(
                "wrong key FAIL — the pane's data came back under a key the machine does not \
                 hold (rows={}): the pin is advisory",
                series.rows.len()
            )))
        }
        Err(SessionFfiError::Peer(sentence))
            if sentence.contains("was opened under the pinned key") =>
        {
            say(&format!("wrong key refused: {sentence}"));
        }
        Err(other) => {
            return Err(Failure::check(format!(
                "wrong key FAIL — refused, but not by the conversation's own pin check: {other:?}"
            )))
        }
    }

    // ---- an empty window is an answer ----
    //
    // A pane that just started has no history, and the read returns an empty
    // series rather than an error (the CLI prints "no history for …" and exits
    // 0). The window ends at the pane's own birth, so this is deterministic
    // rather than a race with the sampler: a row for it can only exist from a
    // tick after it was spawned.
    let born = now_ms();
    let socket = world.socket();
    world.cli(&[
        "spawn",
        FRESH,
        "/bin/sh",
        "-c",
        IDLE_SCRIPT,
        "--socket",
        &socket,
    ])?;
    let empty = session
        .metrics_history(
            peer.clone(),
            daemon_key.clone(),
            FRESH.to_string(),
            born.saturating_sub(300_000),
            born,
            0,
        )
        .await
        .map_err(|e| {
            Failure::check(format!(
                "empty window FAIL — a pane with no history returned an error: {e}"
            ))
        })?;
    if !empty.rows.is_empty() {
        return Err(Failure::check(format!(
            "empty window FAIL — a pane spawned this instant answered with {} rows",
            empty.rows.len()
        )));
    }
    // The empty answer must still be the *history* path's answer: the tier the
    // machine served, with the downshift it reports. The unknown-pane path
    // answers the ask back (`step_ms` = the ask, `downshifted` = false), so this
    // pair is what distinguishes "no rows yet" from "no such pane".
    if empty.step_ms != TIER_MS || !empty.downshifted {
        return Err(Failure::check(format!(
            "empty window FAIL — the empty answer carried step_ms={} downshifted={}, not the \
             tier the machine served",
            empty.step_ms, empty.downshifted
        )));
    }
    summary.empty_rows = empty.rows.len();
    say(&format!(
        "empty window ok step_ms={} downshifted={} rows={} (a state, not an error)",
        empty.step_ms,
        empty.downshifted,
        empty.rows.len()
    ));

    // ---- the act that is taken: an owner answers the blocked pane ----
    //
    // The same conversation, one more verb: a read, then the act, then another
    // read — because an act that broke the conversation would leave the meter
    // dead for the rest of the session.
    let owner = world.pin(OWNER_SEED, OWNER_NAME, FfiRole::Owner, 3)?;
    let owner_session = dial(world, &owner, "owner").await?;
    let (owner_peer, owner_key) = world.await_row(&owner_session, "owner").await?;
    let opened = read(
        &owner_session,
        &owner_peer,
        &owner_key,
        PANE,
        (now.saturating_sub(300_000), u64::MAX),
        0,
        ("owner read #1", "the owner's FIRST read failed"),
    )
    .await?;
    served(&opened, "owner read #1")?;
    summary.took(&opened);

    // The pane becomes `question` from the engine's own reading of its quiet,
    // prompt-shaped tail, so the first attempt can land before the engine has
    // classified it. The daemon's refusal says exactly which state it saw, and a
    // reply is only gated on `question`, so retrying is the honest wait.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match owner_session
            .notify_act(
                owner_peer.clone(),
                owner_key.clone(),
                ASKING.to_string(),
                WireNotifyAction::Reply,
                Some("y".to_string()),
            )
            .await
        {
            Ok(()) => {
                summary.acts += 1;
                say("owner reply taken on the same conversation");
                break;
            }
            Err(SessionFfiError::Daemon(sentence)) if sentence.contains("is not asking") => {
                if Instant::now() >= deadline {
                    return Err(Failure::check(format!(
                        "act FAIL — {ASKING} never reached `question` within 30 s; last: {sentence}"
                    )));
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Err(SessionFfiError::Daemon(sentence)) => {
                return Err(Failure::check(format!(
                    "act FAIL — the machine refused the owner's reply: {sentence}"
                )))
            }
            Err(other) => {
                return Err(Failure::check(format!(
                    "act FAIL — the reply never reached the machine: {other:?}"
                )))
            }
        }
    }

    let after = read(
        &owner_session,
        &owner_peer,
        &owner_key,
        PANE,
        (now.saturating_sub(60_000), u64::MAX),
        0,
        ("owner read #2", "the read after the act failed"),
    )
    .await?;
    served(&after, "owner read #2")?;
    summary.took(&after);
    say(&format!(
        "owner read #2 ok rows={} (the act did not disturb the conversation)",
        after.rows.len()
    ));

    // The answer landed where a phone's user would see it: the pane's own
    // transcript, read through the real CLI (the boundary has no pane-read door).
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut transcript = String::new();
    while Instant::now() < deadline {
        transcript = world.cli(&["read", ASKING, "--socket", &socket])?;
        if transcript.contains("ANSWER=y") {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    if !transcript.contains("ANSWER=y") {
        return Err(Failure::check(format!(
            "act FAIL — the pane never read the answer back: {}",
            first_line(&transcript)
        )));
    }
    say("the pane read the answer back (ANSWER=y) — the act reached the pane");

    // ---- cleanup through the product's own door ----
    //
    // The daemon is killed by the harness when this returns, and a pane outlives
    // its daemon, so the panes are ended through the act door — which is also
    // T-0115's third action, against real processes.
    for pane in [PANE, FRESH, ASKING] {
        owner_session
            .notify_act(
                owner_peer.clone(),
                owner_key.clone(),
                pane.to_string(),
                WireNotifyAction::Kill,
                None,
            )
            .await
            .map_err(|e| Failure::check(format!("kill FAIL — {pane} was not ended: {e}")))?;
        summary.kills += 1;
    }
    say("the owner ended all three panes through the act door");

    Ok(summary)
}

/// One metrics read, named by the check it is.
///
/// `check` is the two phrases the failure carries — `("read #2", "the SECOND
/// read failed")` — because the red this slice must produce is that sentence on
/// real processes (T-0114's M1'), not a generic "assertion failed".
async fn read(
    session: &Arc<RelaySessionHandle>,
    peer: &Arc<RelayPeerHandle>,
    key: &str,
    pane: &str,
    window: (u64, u64),
    step_ms: u64,
    check: (&str, &str),
) -> Outcome<MetricsSeriesInfo> {
    let (name, what) = check;
    session
        .metrics_history(
            peer.clone(),
            key.to_string(),
            pane.to_string(),
            window.0,
            window.1,
            step_ms,
        )
        .await
        .map_err(|e| Failure::check(format!("{name} FAIL — {what}: {e}")))
}

/// The two facts a meter must be able to trust, asserted on every real answer:
/// the rows are the machine's own, and the tier it names is a tier it serves —
/// never the ask echoed back.
fn served(series: &MetricsSeriesInfo, check: &str) -> Outcome<()> {
    if series.rows.is_empty() {
        return Err(Failure::check(format!(
            "{check} FAIL — the machine served an empty series for a pane that has been \
             running for a minute"
        )));
    }
    if !REAL_TIERS.contains(&series.step_ms) {
        return Err(Failure::check(format!(
            "{check} FAIL — step_ms={} is not a tier the machine serves: {series:?}",
            series.step_ms
        )));
    }
    if series.step_ms != TIER_MS || !series.downshifted {
        return Err(Failure::check(format!(
            "{check} FAIL — step_ms={} downshifted={} for a step-0 ask over a short window: \
             the finest tier is what the machine serves there",
            series.step_ms, series.downshifted
        )));
    }
    if series.rows.iter().all(|row| row.rss_peak == 0) {
        return Err(Failure::check(format!(
            "{check} FAIL — every row reports 0 bytes of RSS: not a sample of a live pane"
        )));
    }
    Ok(())
}

/// The machine's own refusal for a viewer sending — built from the core's rules
/// rather than typed, so this cannot drift from what the daemon answers.
///
/// The daemon answers with the *record* of the denial (`VerbDenial::Role`:
/// "device dev_… (viewer) denied: …"), which wraps the gate's sentence
/// (`RoleError::Denied`, from `role::check`). Both halves come from the core
/// here, so a change to either wording moves this assertion with it.
fn denial_for_a_viewer_sending(device: &str) -> Outcome<String> {
    use arreo_core::identity::{DeviceId, Role, VerbDenial};

    let gate =
        arreo_core::identity::role::check(Role::Viewer, arreo_core::identity::role::Verb::Send)
            .expect_err("a viewer may not send");
    let device = DeviceId::parse(device)
        .map_err(|e| Failure::check(format!("the viewer's own device id: {e}")))?;
    Ok(VerbDenial::Role {
        device,
        role: Role::Viewer,
        source: gate,
    }
    .to_string())
}

/// The world the check drives: paths, processes, and the identities it wrote.
struct World {
    /// `target/debug`: the product's own binaries, spawned and never linked.
    debug: PathBuf,
    /// `target/test-scratch/T-0125/real-daemon`: this run's whole world.
    root: PathBuf,
    relay_addr: SocketAddr,
    children: Vec<Child>,
}

impl World {
    fn start() -> Outcome<Self> {
        let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .ok_or_else(|| {
                Failure::check("this crate is not two levels below the workspace root")
            })?;
        let root = workspace
            .join("target")
            .join("test-scratch")
            .join("T-0125")
            .join("real-daemon");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).map_err(|e| {
            Failure::skip(format!(
                "the scratch tree {} cannot be created: {e}",
                root.display()
            ))
        })?;
        let mut world = Self {
            debug: workspace.join("target").join("debug"),
            root,
            relay_addr: "127.0.0.1:0".parse().expect("a literal address"),
            children: Vec::new(),
        };
        world.relay_addr = world.start_relay()?;
        world.register_account()?;
        world.install_machine()?;
        world.start_daemon()?;
        world.spawn_panes()?;
        Ok(world)
    }

    /// A product binary, spawned as a process.
    ///
    /// **Neither the relay nor the server is a dependency of this crate**, and
    /// that is the point: the relay is AGPL (no Apache work may link it) and the
    /// server is the daemon this boundary must talk to, not call into. A missing
    /// binary is a SKIP naming it, never a failure of the product.
    fn binary(&self, name: &str) -> Outcome<PathBuf> {
        let path = self.debug.join(name);
        if path.exists() {
            Ok(path)
        } else {
            Err(Failure::skip(format!(
                "{} is missing — build it first: \
                 `cargo build -p arreo-cli -p arreo-server -p arreo-relay`",
                path.display()
            )))
        }
    }

    fn socket(&self) -> String {
        self.machine_dir().join("arreo.sock").display().to_string()
    }

    fn machine_dir(&self) -> PathBuf {
        self.root.join("machine")
    }

    /// A running `arreo-relay`, on the address it announces.
    fn start_relay(&mut self) -> Outcome<SocketAddr> {
        let relay = self.binary("arreo-relay")?;
        let state = self.root.join("relay");
        fs::create_dir_all(&state)
            .map_err(|e| Failure::skip(format!("the relay's state dir: {e}")))?;
        let log = state.join("relay.log");
        let mut child = Command::new(&relay)
            .args([
                "serve",
                "--listen",
                "127.0.0.1:0",
                "--state-dir",
                &state.display().to_string(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Failure::skip(format!("`{} serve` cannot start: {e}", relay.display())))?;
        let stderr = child.stderr.take().expect("stderr was piped");
        let (ready_tx, ready_rx) = mpsc::channel();
        let log_path = log.clone();
        std::thread::spawn(move || {
            let mut file = fs::File::create(&log_path).expect("the relay's log");
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let _ = writeln!(file, "{line}");
                if let Some(rest) = line.split("router on ").nth(1) {
                    if let Some(addr) = rest.split_whitespace().next() {
                        if let Ok(addr) = addr.parse::<SocketAddr>() {
                            let _ = ready_tx.send(addr);
                        }
                    }
                }
            }
        });
        let announced = ready_rx.recv_timeout(Duration::from_secs(20));
        self.children.push(child);
        match announced {
            Ok(addr) => Ok(addr),
            // A relay that exited is one this machine cannot run (no QUIC, no
            // route, a sandbox); one that is still alive but silent has drifted
            // from the line this reads, which is a failure, not a skip.
            Err(_) => {
                let tail = tail_of(&log);
                match self.children.last_mut().and_then(|c| c.try_wait().ok().flatten()) {
                    Some(status) => Err(Failure::skip(format!(
                        "`arreo-relay serve` exited ({status}) without announcing a listener; \
                         log tail: {tail}"
                    ))),
                    None => Err(Failure::check(format!(
                        "`arreo-relay serve` never announced a listener within 20 s; log tail: {tail}"
                    ))),
                }
            }
        }
    }

    /// Register the account with the relay, through the real CLI verb.
    fn register_account(&self) -> Outcome<()> {
        let relay = self.binary("arreo-relay")?;
        // The account's **public** root key, read through the boundary: the
        // secret never leaves the seed this test built the handle from.
        let root = root_key_from_seed(ACCOUNT_SEED.to_vec())
            .map_err(|e| Failure::check(format!("a root from a seed: {e}")))?;
        let out = Command::new(&relay)
            .args([
                "account",
                "add",
                "--state-dir",
                &self.root.join("relay").display().to_string(),
                "--account",
                ACCOUNT,
                "--root-key",
                &root.public_hex(),
            ])
            .output()
            .map_err(|e| Failure::skip(format!("`arreo-relay account add` cannot run: {e}")))?;
        if !out.status.success() {
            return Err(Failure::check(format!(
                "registering the account on the relay failed: {}",
                first_line(&String::from_utf8_lossy(&out.stderr))
            )));
        }
        Ok(())
    }

    /// The machine's identity directory, written the way `arreo pair` leaves it.
    ///
    /// The key files are 64 hex characters and the certificate is its encoded
    /// bytes — MessagePack, not text — and both are owner-only because
    /// `DeviceKey::load` refuses a file anyone else can read.
    fn install_machine(&self) -> Outcome<()> {
        let root = root_key_from_seed(ACCOUNT_SEED.to_vec())
            .map_err(|e| Failure::check(format!("a root from a seed: {e}")))?;
        let machine = device_key_from_seed(MACHINE_SEED.to_vec())
            .map_err(|e| Failure::check(format!("a key from a seed: {e}")))?;
        let public = machine.public_hex();
        let cert = device_cert_issue(
            root,
            public.clone(),
            MACHINE_NAME.to_string(),
            FfiRole::Owner,
            1_000,
            1,
        )
        .map_err(|e| Failure::check(format!("issuing the machine's certificate: {e}")))?;
        let identity = self.machine_dir().join("identity");
        fs::create_dir_all(identity.join("devices"))
            .map_err(|e| Failure::skip(format!("the machine's identity dir: {e}")))?;
        write_private(&identity.join("root.key"), hex(&ACCOUNT_SEED).as_bytes())?;
        write_private(&identity.join("device.key"), hex(&MACHINE_SEED).as_bytes())?;
        let fingerprint = fingerprint_of_public_key(public)
            .map_err(|e| Failure::check(format!("a fingerprint: {e}")))?;
        let encoded = cert
            .encode()
            .map_err(|e| Failure::check(format!("the certificate encodes: {e}")))?;
        write_private(
            &identity.join("devices").join(format!("{fingerprint}.cert")),
            &encoded,
        )?;
        fs::write(
            self.machine_dir().join("arreo.toml"),
            format!(
                "[relay]\nenabled = true\naddr = \"{}\"\naccount = \"{ACCOUNT}\"\nname = \"{MACHINE_NAME}\"\n",
                self.relay_addr
            ),
        )
        .map_err(|e| Failure::skip(format!("the machine's config: {e}")))?;
        Ok(())
    }

    /// Start the machine's daemon and wait for its socket.
    fn start_daemon(&mut self) -> Outcome<()> {
        let server = self.binary("arreo-server")?;
        let dir = self.machine_dir();
        let socket = dir.join("arreo.sock");
        let log = dir.join("daemon.log");
        let stderr =
            fs::File::create(&log).map_err(|e| Failure::skip(format!("the daemon's log: {e}")))?;
        let child = Command::new(&server)
            .arg("--socket")
            .arg(&socket)
            .arg("--config")
            .arg(dir.join("arreo.toml"))
            .env("ARREO_IDENTITY_DIR", &dir)
            .stdout(Stdio::null())
            .stderr(Stdio::from(stderr))
            .spawn()
            .map_err(|e| Failure::skip(format!("`{}` cannot start: {e}", server.display())))?;
        self.children.push(child);
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if std::os::unix::net::UnixStream::connect(&socket).is_ok() {
                return Ok(());
            }
            if let Some(status) = self
                .children
                .last_mut()
                .and_then(|c| c.try_wait().ok().flatten())
            {
                return Err(Failure::skip(format!(
                    "`arreo-server` exited ({status}) without binding {}; log tail: {}",
                    socket.display(),
                    tail_of(&log)
                )));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Err(Failure::check(format!(
            "`arreo-server` did not bind {} within 20 s; log tail: {}",
            socket.display(),
            tail_of(&log)
        )))
    }

    /// The two panes the checks are about, spawned through the real CLI.
    fn spawn_panes(&self) -> Outcome<()> {
        let socket = self.socket();
        self.cli(&[
            "spawn",
            PANE,
            "/bin/sh",
            "-c",
            IDLE_SCRIPT,
            "--socket",
            &socket,
        ])?;
        self.cli(&[
            "spawn",
            ASKING,
            "/bin/sh",
            "-c",
            ASKING_SCRIPT,
            "--socket",
            &socket,
        ])?;
        Ok(())
    }

    /// The machine pins a phone, through the real CLI — the one step a
    /// deployment does by pairing — and hands back the key and the certificate
    /// the boundary issued for it.
    fn pin(&self, seed: [u8; 32], name: &str, role: FfiRole, serial: u64) -> Outcome<Phone> {
        let root = root_key_from_seed(ACCOUNT_SEED.to_vec())
            .map_err(|e| Failure::check(format!("a root from a seed: {e}")))?;
        let device = device_key_from_seed(seed.to_vec())
            .map_err(|e| Failure::check(format!("a key from a seed: {e}")))?;
        let public = device.public_hex();
        let cert = device_cert_issue(root, public.clone(), name.to_string(), role, 1_000, serial)
            .map_err(|e| Failure::check(format!("issuing {name}'s certificate: {e}")))?;
        let socket = self.socket();
        self.cli(&[
            "devices",
            "issue",
            "--socket",
            &socket,
            "--name",
            name,
            "--role",
            &role_word(role),
            "--key",
            &public,
        ])?;
        let id = format!(
            "dev_{}",
            fingerprint_of_public_key(public)
                .map_err(|e| Failure::check(format!("a fingerprint: {e}")))?
        );
        Ok(Phone { device, cert, id })
    }

    /// Run the real CLI as the machine.
    fn cli(&self, args: &[&str]) -> Outcome<String> {
        let cli = self.binary("arreo")?;
        let dir = self.machine_dir();
        let out = Command::new(&cli)
            .args(args)
            .env("ARREO_IDENTITY_DIR", &dir)
            .env("ARREO_CONFIG", dir.join("arreo.toml"))
            .output()
            .map_err(|e| Failure::skip(format!("`{}` cannot run: {e}", cli.display())))?;
        let mut text = String::from_utf8_lossy(&out.stdout).to_string();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        if !out.status.success() {
            return Err(Failure::check(format!(
                "`arreo {}` failed ({:?}): {}",
                args.join(" "),
                out.status.code(),
                first_line(&text)
            )));
        }
        Ok(text)
    }

    /// Wait for the machine's row, read through the exported directory door.
    async fn await_row(
        &self,
        session: &Arc<RelaySessionHandle>,
        who: &str,
    ) -> Outcome<(Arc<RelayPeerHandle>, String)> {
        let deadline = Instant::now() + Duration::from_secs(40);
        let mut last = String::from("no directory answer yet");
        while Instant::now() < deadline {
            let reply = session.machines(false).await.map_err(|e| {
                Failure::check(format!("directory FAIL — the {who}'s read failed: {e}"))
            })?;
            if let Some(row) = reply.machines.iter().find(|row| row.name == MACHINE_NAME) {
                if let Some(key) = row.daemon_key.clone() {
                    // **The peer is the device id of the key the row publishes**,
                    // not the row's `machine_id`: the relay routes by the id the
                    // daemon *dialed* with, while `machine_id` is the machine's
                    // directory identity (its root key, T-0043 — the thing that
                    // outlives re-pairing). The core's own resolver draws the same
                    // line (`DeviceId::from_key(&server_key)`), and a stream opened
                    // to `machine_id` reaches no device at all.
                    let fingerprint = fingerprint_of_public_key(key.clone())
                        .map_err(|e| Failure::check(format!("the machine's directory key: {e}")))?;
                    let peer = relay_peer_parse(fingerprint)
                        .map_err(|e| Failure::check(format!("the machine's device id: {e}")))?;
                    return Ok((peer, key));
                }
                last = format!("a row without a daemon_key: {row:?}");
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        Err(Failure::check(format!(
            "directory FAIL — no row for {MACHINE_NAME} within 40 s; last: {last}"
        )))
    }

    fn shutdown(&mut self) {
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// A phone: the key the boundary built from a seed, the certificate it issued
/// for it, and the id the machine pinned.
struct Phone {
    device: Arc<DeviceKeyHandle>,
    cert: Arc<DeviceCertHandle>,
    id: String,
}

/// Dial the relay as this phone, through the exported door.
async fn dial(world: &World, phone: &Phone, who: &str) -> Outcome<Arc<RelaySessionHandle>> {
    let session = relay_session_dial(
        world.relay_addr.to_string(),
        ACCOUNT.to_string(),
        phone.device.clone(),
        phone.cert.clone(),
    )
    .await
    .map_err(|e| Failure::check(format!("dial FAIL — the {who}'s session was refused: {e}")))?;
    say(&format!(
        "{who} {} dialed account={} (session device_id={})",
        phone.id,
        session.account(),
        session.device_id()
    ));
    Ok(session)
}

/// Write a file the way the identity store does: owner-only, parent dir too.
fn write_private(path: &Path, bytes: &[u8]) -> Outcome<()> {
    fs::write(path, bytes).map_err(|e| Failure::skip(format!("{}: {e}", path.display())))?;
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    if let Some(parent) = path.parent() {
        let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

/// One line of the check's narrative. The prefix is added here rather than at
/// every call site, so the runner's evidence filter has exactly one spelling to
/// match and a message that needs no values is a plain literal.
fn say(message: &str) {
    println!("{SLICE}: {message}");
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The last few lines of a process's log, for a failure or a skip that has to
/// name why.
fn tail_of(path: &Path) -> String {
    let text = fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let tail = lines[lines.len().saturating_sub(5)..].join(" | ");
    if tail.is_empty() {
        "(no output)".to_string()
    } else {
        tail
    }
}

/// The first non-empty line: a failure sentence stays one line in a report.
fn first_line(text: &str) -> String {
    text.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("(no output)")
        .trim()
        .to_string()
}
