//! The relay router (T-0029): authenticate a device, then move its bytes.
//!
//! One sentence: a device completes the nonce handshake, is registered under
//! `(account, device)`, and every envelope it sends is checked against that
//! session and forwarded to its destination — or answered with a typed outcome
//! — while the payload is never decoded.
//!
//! **What the relay can and cannot do**, because "routes bytes it cannot read"
//! is a claim that has to be checkable:
//! - It *can* read the header: account, sender, destination, sequence, kind.
//!   That is what routing needs, and the schema deliberately has nowhere to put
//!   pane text, agent state or a key (a test asserts the column set).
//! - It *cannot* read the payload. [`RelayEnvelope::decode`] stops at the end of
//!   the header and hands the rest on as bytes, so no relay-side type ever
//!   holds a payload's meaning — and the integration test asserts that
//!   pane-shaped content arrives byte-identical and appears nowhere on disk.
//! - It does not *decide* identity: [`arreo_core::relay::verify_auth`] does, in
//!   core, and the router only carries the result. That is what keeps the trust
//!   decision in one reviewed function rather than spread across a socket loop.

use crate::directory::{Directory, JoinTicket};
use crate::inbox::{Drained, Inbox, InboxError, InboxLimits};
use crate::store::{RelayStore, StoreError};
use arreo_core::identity::{DeviceId, VerifyingKey};
use arreo_core::mesh::{MachineId, Name};
use arreo_core::relay::join_proof_payload;
use arreo_core::relay::{
    decode_message, decode_payload, encode_message, encode_payload, fresh_nonce, read_envelope,
    read_frame, verify_auth, write_frame, Ack, Auth, AuthReply, DirectoryReply, DrainReport,
    DrainRequest, Hello, HelloReply, JoinRequest, MachinesRequest, Outcome, RelayEnvelope,
    RelayError, RelayHeader, RelayKind, RemoveRequest, RenameRequest, StaleRequest,
    MAX_HANDSHAKE_BYTES, RELAY_SENDER, RELAY_VERSION,
};
use arreo_core::transport::{
    accept_connection, Accepted, Connection, Endpoint, HandshakeLimiter, QuicError,
};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// The address the relay serves on when none is given (loopback: the shipped
/// posture is a self-hosted relay behind the operator's own network).
pub const DEFAULT_LISTEN: &str = "127.0.0.1:8787";

/// How many outbound items one connection may have queued before the relay
/// treats it as not keeping up.
///
/// A device that stops reading must not make the relay buffer without bound —
/// that is how one slow peer becomes everybody's problem. When the queue is
/// full the destination counts as unreachable and the sender is told so.
pub const OUTBOUND_QUEUE: usize = 64;

/// The relay's failures, as the router reports them.
#[derive(Debug, thiserror::Error)]
pub enum RouterError {
    #[error("relay store: {0}")]
    Store(#[from] StoreError),
    #[error("relay inbox: {0}")]
    Inbox(#[from] InboxError),
    #[error("relay transport: {0}")]
    Transport(#[from] QuicError),
    #[error("relay protocol: {0}")]
    Protocol(#[from] RelayError),
}

/// One authenticated session's identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub account_id: String,
    pub device_id: DeviceId,
    /// The nonce this session's handshake issued. Kept alive past the handshake
    /// because a `join` proof is signed over it (T-0056): replaying a recorded
    /// join into a *different* session must fail, and binding the proof to the
    /// session's own challenge is what makes that true without a second
    /// challenge.
    pub nonce: Vec<u8>,
    /// The public key this session proved possession of (T-0045). Kept because the
    /// machine directory publishes it as the key a peer dials: the relay learned
    /// it from the certificate it verified, so a row cannot advertise a route the
    /// machine does not hold.
    pub public_key: VerifyingKey,
}

impl Session {
    /// The key every map in this module uses. One spelling, so a lookup cannot
    /// miss because one side wrote `dev_<hex>` and the other the bare hex — the
    /// defect T-0023 hit in the transport's resolver.
    fn key(&self) -> (String, String) {
        (self.account_id.clone(), self.device_id.as_str().to_string())
    }

    /// The dial key, in the hex spelling the directory stores.
    fn dial_key(&self) -> String {
        let mut out = String::with_capacity(64);
        for byte in self.public_key.to_bytes() {
            out.push_str(&format!("{byte:02x}"));
        }
        out
    }
}

/// Something to write to a device.
#[derive(Debug)]
enum Outbound {
    /// An envelope for this device.
    Envelope(RelayEnvelope),
    /// This device's report on one of its own envelopes.
    Status { seq: u64, outcome: Outcome },
    /// A drained message, replayed exactly as it was stored: the relay holds the
    /// framed bytes and never rebuilds them, because rebuilding would mean
    /// decoding a header it is not supposed to read.
    Raw(Vec<u8>),
    /// A drain report.
    Drain(DrainReport),
    /// This device's peer went offline (T-0054): the named device is the one that
    /// left.
    PeerGone(DeviceId),
    /// The answer to a `join` or `machines` request (T-0056).
    Directory(DirectoryReply),
    /// Stop: a newer session for this device took the route (T-0060).
    ///
    /// Not a message for the peer — there is nothing to write. It exists so the
    /// *relay* can end a session it has stopped routing to, rather than leaving it
    /// connected and silently ignored. The writer returns on this, which closes the
    /// stream, and the peer's client reconnects on the path it already has.
    Displaced,
}

/// The live sessions, keyed by `(account, device)`.
struct Live {
    senders: HashMap<(String, String), (mpsc::Sender<Outbound>, u64)>,
    next_token: u64,
}

/// The router: the store, the inbox, the live sessions, and the handshake budget.
pub struct Router {
    store: RelayStore,
    inbox: Inbox,
    live: Mutex<Live>,
    limiter: HandshakeLimiter,
}

impl Router {
    #[must_use]
    pub fn new(store: RelayStore, limits: InboxLimits) -> Self {
        let inbox = Inbox::new(store.clone(), limits);
        Self {
            store,
            inbox,
            live: Mutex::new(Live {
                senders: HashMap::new(),
                next_token: 1,
            }),
            limiter: HandshakeLimiter::default(),
        }
    }

    /// Append an audit row, and never let the trail take the relay down with it.
    ///
    /// A failure to record is logged and dropped rather than propagated. The
    /// alternative — refusing a session because the write failed — turns a full
    /// disk into an outage, and the trail is a record *of* the service, not a
    /// precondition for it. The cost is a real one and worth naming: on a full
    /// disk the relay keeps serving and the trail has a gap, which the log line
    /// marks. The other direction (a relay nobody can connect to, with a perfect
    /// trail) is worse for the person running it.
    pub fn record(&self, event: crate::audit::RelayAuditEvent) {
        if let Err(e) = self.store.record(&event) {
            eprintln!(
                "arreo-relay: cannot write the audit trail ({}): {e}",
                event.action
            );
        }
    }

    #[must_use]
    pub fn store(&self) -> &RelayStore {
        &self.store
    }

    #[must_use]
    pub fn inbox(&self) -> &Inbox {
        &self.inbox
    }

    /// How many devices are connected right now (tests, and the operator's log).
    #[must_use]
    pub fn live_count(&self) -> usize {
        self.lock_live().senders.len()
    }

    /// Presence for every device the relay has ever seen in `account_id`
    /// (T-0031): the stored `last_seen_ms` plus the one rule applied at `now_ms`.
    ///
    /// The rule is applied here, at the read, so a `kill -9` + restart changes
    /// nothing: presence is recomputed from storage, and nothing reads `online`
    /// until it reconnects — there is no phantom state to clear, because there
    /// is no live flag stored anywhere.
    pub fn presence(
        &self,
        account_id: &str,
        now_ms: i64,
    ) -> Result<Vec<crate::presence::DevicePresence>, StoreError> {
        Ok(self
            .store
            .device_presence(account_id)?
            .into_iter()
            .map(
                |(device_id, last_seen_ms)| crate::presence::DevicePresence {
                    device_id,
                    account_id: account_id.to_string(),
                    presence: crate::presence::presence_at(last_seen_ms, now_ms),
                    last_seen_ms,
                },
            )
            .collect())
    }

    fn lock_live(&self) -> std::sync::MutexGuard<'_, Live> {
        match self.live.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Register a session's outbound queue, returning the token that must be
    /// used to deregister it.
    ///
    /// The token exists because a device may reconnect while its old connection
    /// is still winding down: without it, the old connection's cleanup would
    /// remove the *new* one's registration and silently make the device
    /// unreachable.
    fn register(&self, session: &Session, sender: mpsc::Sender<Outbound>) -> u64 {
        let mut live = self.lock_live();
        let token = live.next_token;
        live.next_token += 1;
        let displaced = live.senders.insert(session.key(), (sender, token));
        if let Some((previous, _)) = displaced {
            // **A replaced route must be told, not just dropped** (T-0060). The map
            // holds one entry per device, so a second session for the same device —
            // a CLI verb run on the machine that hosts the daemon, say — takes the
            // route. The replaced session's own reader keeps a sender clone alive, so
            // nothing about it would ever end on its own: it would sit connected and
            // unrouted, and the machine would be unreachable until something else
            // happened to restart its daemon.
            //
            // `try_send` because this runs under the registry lock: a session whose
            // queue is full is not reading its socket anyway, and the notice is a
            // courtesy that must not block every other device's registration.
            let _ = previous.try_send(Outbound::Displaced);
        }
        token
    }

    fn deregister(&self, session: &Session, token: u64) {
        let notified: Vec<mpsc::Sender<Outbound>> = {
            let mut live = self.lock_live();
            // The token check: a device that reconnected while its old
            // connection was winding down must not be removed by the old
            // connection's cleanup. And a departure is only news if *this*
            // connection was the live one — otherwise the device is still here
            // under a newer registration.
            let was_live = live
                .senders
                .get(&session.key())
                .is_some_and(|(_, held)| *held == token);
            if !was_live {
                return;
            }
            live.senders.remove(&session.key());
            // Everyone else in the account, because the relay does not track who
            // holds a stream to whom (T-0054): the notice is account-wide news and
            // the receivers decide whether they cared.
            live.senders
                .iter()
                .filter(|((account, device), _)| {
                    *account == session.account_id && *device != session.device_id.as_str()
                })
                .map(|(_, (sender, _))| sender.clone())
                .collect()
        };
        // Outside the lock: a full queue must not hold the registry while it
        // drains, and a device that is behind misses the notice rather than
        // blocking every other device's disconnect.
        for sender in notified {
            let _ = sender.try_send(Outbound::PeerGone(session.device_id.clone()));
        }
    }

    /// Answer a machine's directory assertion (T-0056).
    ///
    /// Two cases, one kind: a machine the account already has refreshes its
    /// presence (no ticket, no name rule — a reconnect must not need an
    /// operator), and a machine the account does not have claims a name through
    /// a ticket the relay mints here, applying T-0043's collision rule. The proof
    /// is checked first either way, so a machine cannot refresh or claim a row
    /// for a key it does not hold.
    fn join(&self, session: &Session, request: &JoinRequest, seq: u64) -> DirectoryReply {
        let refuse = |reason: String| DirectoryReply {
            v: RELAY_VERSION,
            seq,
            granted: None,
            machines: Vec::new(),
            refused: Some(reason),
        };

        if request.v != RELAY_VERSION {
            return refuse(format!(
                "join speaks version {} (this relay speaks {RELAY_VERSION})",
                request.v
            ));
        }
        let Ok(key) = arreo_core::identity::keys::public_from_hex(&request.machine_key) else {
            return refuse(format!(
                "machine key {:?} is not a 32-byte hex public key",
                request.machine_key
            ));
        };
        if !verify_join_proof(
            &key,
            &session.nonce,
            &session.account_id,
            &request.machine_key,
            &request.name,
            &request.signature,
        ) {
            return refuse(
                "the join proof does not verify: the signature is not by this machine key over \
                 this session's challenge"
                    .to_string(),
            );
        }
        let machine = MachineId::from_key(&key);
        let directory = Directory::new(self.store.clone());
        let now = crate::directory::now_ms();

        // Existing row: refresh presence. The name is *not* re-applied — a
        // returning machine keeps the name it was granted, including a suffix it
        // did not ask for.
        let mine = directory
            .list(&session.account_id, now)
            .map(|rows| rows.into_iter().find(|row| row.machine_id == machine));
        match mine {
            Ok(Some(_)) => {
                if let Err(e) = directory.heartbeat(&machine, now) {
                    return refuse(format!("could not refresh the row: {e}"));
                }
                return match directory.list(&session.account_id, now) {
                    Ok(rows) => DirectoryReply {
                        v: RELAY_VERSION,
                        seq,
                        granted: rows.into_iter().find(|row| row.machine_id == machine),
                        machines: Vec::new(),
                        refused: None,
                    },
                    Err(e) => refuse(format!("could not read the row back: {e}")),
                };
            }
            Ok(None) => {}
            Err(e) => return refuse(format!("could not read the directory: {e}")),
        }

        let name = match Name::parse(&request.name) {
            Ok(name) => name,
            Err(e) => return refuse(format!("{:?} is not a machine name: {e}", request.name)),
        };
        let ticket = JoinTicket::issue(&session.account_id, now);
        // The dial key is the **authenticated** device's key, not anything the
        // request carried: this session already proved it holds that key (chain +
        // proof of possession), so a machine cannot advertise a route it does not
        // control.
        let daemon_key = session.dial_key();
        match directory.join(
            ticket,
            &machine,
            &name,
            request.proto_version,
            &daemon_key,
            now,
        ) {
            Ok(row) => DirectoryReply {
                v: RELAY_VERSION,
                seq,
                granted: Some(row),
                machines: Vec::new(),
                refused: None,
            },
            Err(e) => refuse(format!("the relay would not admit this machine: {e}")),
        }
    }

    /// Does this session's account list that machine?
    ///
    /// The authorization check for the write verbs: the directory is the
    /// account's, and the session authenticated as a device of one account. One
    /// extra read per write — writes are operator actions, not a hot path.
    fn owns(&self, session: &Session, machine: &MachineId) -> bool {
        Directory::new(self.store.clone())
            .list(&session.account_id, crate::directory::now_ms())
            .is_ok_and(|rows| rows.iter().any(|row| &row.machine_id == machine))
    }

    /// Rename a machine (T-0057).
    ///
    /// The directory's own rule decides — this function decodes, calls, and
    /// maps the outcome to a refusal string, so a client cannot hold a different
    /// opinion about whether a name was free.
    fn rename(&self, session: &Session, request: &RenameRequest, seq: u64) -> DirectoryReply {
        if request.v != RELAY_VERSION {
            return refuse(seq, format!("rename speaks version {}", request.v));
        }
        let Ok(machine) = MachineId::parse(&request.machine_id) else {
            return refuse(seq, format!("{:?} is not a machine id", request.machine_id));
        };
        let Ok(name) = Name::parse(&request.new_name) else {
            return refuse(seq, format!("{:?} is not a machine name", request.new_name));
        };
        // The machine must belong to *this* session's account. The directory's
        // rows are keyed by `machine_id`, which is unique across accounts, so
        // without this check any account's device could rename or tombstone
        // another account's machine by guessing an id — and the refusal is the
        // same one an unknown id gets, so it does not become an oracle for
        // "this machine exists somewhere else".
        if !self.owns(session, &machine) {
            return refuse(
                seq,
                format!("no machine {} in this account", request.machine_id),
            );
        }
        let directory = Directory::new(self.store.clone());
        match directory.rename(&machine, &name, crate::directory::now_ms()) {
            Ok(row) => DirectoryReply {
                v: RELAY_VERSION,
                seq,
                granted: Some(row),
                machines: Vec::new(),
                refused: None,
            },
            Err(e) => refuse(seq, format!("the rename was refused: {e}")),
        }
    }

    /// Tombstone a machine's name (T-0057).
    fn remove(&self, session: &Session, request: &RemoveRequest, seq: u64) -> DirectoryReply {
        if request.v != RELAY_VERSION {
            return refuse(seq, format!("remove speaks version {}", request.v));
        }
        let Ok(machine) = MachineId::parse(&request.machine_id) else {
            return refuse(seq, format!("{:?} is not a machine id", request.machine_id));
        };
        if !self.owns(session, &machine) {
            return refuse(
                seq,
                format!("no machine {} in this account", request.machine_id),
            );
        }
        let directory = Directory::new(self.store.clone());
        match directory.remove(&machine, crate::directory::now_ms()) {
            Ok(row) => DirectoryReply {
                v: RELAY_VERSION,
                seq,
                granted: Some(row),
                machines: Vec::new(),
                refused: None,
            },
            Err(e) => refuse(seq, format!("the removal was refused: {e}")),
        }
    }

    /// Prune exactly the stale machines (T-0057): T-0043's rule, applied by the
    /// relay, with the pruned rows in the reply so the caller can say what it
    /// reclaimed without a second read.
    fn prune_stale(&self, session: &Session, request: StaleRequest, seq: u64) -> DirectoryReply {
        if request.v != RELAY_VERSION {
            return refuse(seq, format!("stale speaks version {}", request.v));
        }
        let directory = Directory::new(self.store.clone());
        let now = crate::directory::now_ms();
        let pruned = match directory.remove_stale(&session.account_id, now) {
            Ok(pruned) => pruned,
            Err(e) => return refuse(seq, format!("the prune was refused: {e}")),
        };
        // The rows are read back *tombstoned*: a pruned machine keeps its name
        // for the tombstone window, which is exactly what "what it reclaimed"
        // should report.
        let rows = directory
            .list(&session.account_id, now)
            .unwrap_or_default()
            .into_iter()
            .filter(|row| pruned.contains(&row.machine_id))
            .collect();
        DirectoryReply {
            v: RELAY_VERSION,
            seq,
            granted: None,
            machines: rows,
            refused: None,
        }
    }

    /// Answer a directory read (T-0056). Presence comes from the same rule the
    /// export uses — the relay computes it, never the client.
    fn machines(&self, session: &Session, all: bool, seq: u64) -> DirectoryReply {
        let directory = Directory::new(self.store.clone());
        // One reading of the clock for the whole answer: two calls could straddle
        // a tombstone expiring and return a list that is neither "with" nor
        // "without" it.
        let now = crate::directory::now_ms();
        match directory.list(&session.account_id, now) {
            Ok(rows) => DirectoryReply {
                v: RELAY_VERSION,
                seq,
                granted: None,
                machines: rows
                    .into_iter()
                    .filter(|row| all || !row.tombstone_active(now))
                    .collect(),
                refused: None,
            },
            Err(e) => DirectoryReply {
                v: RELAY_VERSION,
                seq,
                granted: None,
                machines: Vec::new(),
                refused: Some(format!("could not read the directory: {e}")),
            },
        }
    }

    /// The decision for one envelope, without sending it.
    ///
    /// Synchronous and free of I/O so the policy is testable without a socket,
    /// and so there is exactly one place where "may this device send this, to
    /// here" is answered. Delivery itself happens in [`read_loop`], with the
    /// real payload — a `decide` that enqueued a placeholder would send the
    /// envelope twice.
    fn decide(&self, session: &Session, header: &RelayHeader) -> Decision {
        // The account is the session's, never the wire's: a device cannot reach
        // into another account by writing a different id in the header.
        if header.account_id != session.account_id {
            return Decision::Refuse(format!(
                "session is for account {}, not {}",
                session.account_id, header.account_id
            ));
        }
        // The sender is the session's. This is the check that makes spoofing
        // impossible rather than merely discouraged.
        match DeviceId::parse(&header.src_device) {
            Ok(claimed) if claimed == session.device_id => {}
            Ok(claimed) => {
                return Decision::Refuse(format!(
                    "session is {}, not {}",
                    session.device_id, claimed
                ))
            }
            Err(e) => return Decision::Refuse(format!("malformed src_device: {e}")),
        }
        // A device sends frames; statuses are the relay's to originate.
        if header.kind != RelayKind::Frame {
            return Decision::Refuse("a device may not send status envelopes".to_string());
        }
        let dst = match DeviceId::parse(&header.dst) {
            Ok(dst) => dst,
            Err(e) => return Decision::Refuse(format!("malformed dst: {e}")),
        };
        // Known-but-offline and never-seen are different answers: the first is
        // "try later" (and becomes a queue in T-0030), the second is a mistake
        // the sender should see immediately.
        match self.store.device_known(&session.account_id, dst.as_str()) {
            Ok(true) => {}
            Ok(false) => return Decision::NoSuchDevice,
            Err(e) => return Decision::Refuse(format!("device registry lookup failed: {e}")),
        }
        let key = (session.account_id.clone(), dst.as_str().to_string());
        match self.lock_live().senders.get(&key) {
            Some((sender, _)) => Decision::Deliver(sender.clone()),
            // Known, not connected: this is the case §3.14 exists for, so it
            // goes to the durable queue rather than being refused (T-0030).
            None => Decision::Queue(dst),
        }
    }
}

/// The router's answer for one envelope.
#[derive(Debug)]
enum Decision {
    /// Hand this envelope to the destination's live queue.
    Deliver(mpsc::Sender<Outbound>),
    /// Commit it to the destination's durable inbox (T-0030).
    Queue(DeviceId),
    NoSuchDevice,
    Refuse(String),
}

/// How often the relay sweeps expired inbox rows on its own.
///
/// The sweep is also lazy (inside enqueue and drain), but a device that never
/// returns would otherwise keep its expired rows — and a self-hosted relay's
/// disk — forever. One hour is frequent enough to bound that and cheap enough to
/// ignore.
pub const SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// When the audit trail is worth telling the operator about: 100 MB of text. The
/// guard only warns — nothing prunes automatically, because a relay that deleted
/// its own trail would defeat the point of keeping one (T-0053).
pub const AUDIT_WARN_BYTES: u64 = 100 * 1024 * 1024;

/// Serve sessions until the endpoint closes.
pub async fn serve(endpoint: Endpoint, router: Arc<Router>) -> Result<(), RouterError> {
    // The periodic sweep, so expiry does not depend on traffic arriving.
    {
        let router = Arc::clone(&router);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(SWEEP_INTERVAL);
            // The first tick is immediate; skip it so a restart does not sweep
            // before the router is serving.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                match router.inbox.sweep(crate::directory::now_ms()) {
                    Ok(0) => {}
                    Ok(expired) => {
                        eprintln!("arreo-relay: inbox sweep expired {expired} message(s)")
                    }
                    Err(e) => eprintln!("arreo-relay: inbox sweep failed: {e}"),
                }
                if let Ok(Some(warning)) = router.store().audit_size_warning(AUDIT_WARN_BYTES) {
                    eprintln!("arreo-relay: {warning}");
                }
            }
        });
    }
    loop {
        // Only the accept is serialized; the peer-paced handshake runs in its
        // own task, so one quiet peer cannot stop the next device connecting
        // (the same split the daemon's transport uses, for the same reason).
        let connection = match accept_connection(&endpoint, &router.limiter).await? {
            Accepted::Open(connection) => connection,
            Accepted::OverBudget(ip) => {
                // The transport logs the refusal, but a log line is not a record:
                // the operator asking "is someone hammering this relay?" needs the
                // trail to answer, and this is the only place that knows *who* was
                // turned away (T-0053).
                //
                // The budget is per *address*, so there is no port to record — and
                // the truncation rule drops ports anyway, so the row says the same
                // thing a full address would.
                router.record(
                    crate::audit::RelayAuditEvent::new(
                        crate::audit::actions::REFUSE,
                        arreo_core::store::AuditOutcome::Refused,
                    )
                    .peer(std::net::SocketAddr::new(ip, 0))
                    .detail("over its handshake budget"),
                );
                continue;
            }
        };
        let router = Arc::clone(&router);
        tokio::spawn(async move {
            if let Err(e) = handle_connection(connection, router).await {
                eprintln!("arreo-relay: session ended: {e}");
            }
        });
    }
}

/// One device's session, start to finish.
async fn handle_connection(connection: Connection, router: Arc<Router>) -> Result<(), RouterError> {
    let peer = connection.remote_address();
    let (mut send, mut recv) = connection
        .accept_bi()
        .await
        .map_err(|e| RouterError::Transport(QuicError::Connect(e.to_string())))?;
    let mut buf = Vec::new();

    // Hello: who is calling, and is that account known at all? An unknown
    // account is refused here, before any cryptography — the cheap answer.
    let body = read_frame(&mut recv, &mut buf, MAX_HANDSHAKE_BYTES).await?;
    let hello: Hello = decode_message(&body)?;
    if hello.v != RELAY_VERSION {
        return refuse_hello(
            &mut send,
            &connection,
            format!("protocol version {} is not supported", hello.v),
        )
        .await;
    }
    let Some(root_bytes) = router.store.account_root(&hello.account_id)? else {
        eprintln!(
            "arreo-relay: refused {peer}: unknown account {}",
            hello.account_id
        );
        router.record(
            crate::audit::RelayAuditEvent::new(
                crate::audit::actions::REFUSE,
                arreo_core::store::AuditOutcome::Refused,
            )
            .account(hello.account_id.clone())
            .device(hello.device_id.clone())
            .peer(peer)
            .proto_version(hello.v)
            .detail("unknown account"),
        );
        return refuse_hello(
            &mut send,
            &connection,
            format!("unknown account {}", hello.account_id),
        )
        .await;
    };
    let root = match VerifyingKey::from_bytes(&root_bytes) {
        Ok(root) => root,
        Err(e) => {
            return refuse_hello(
                &mut send,
                &connection,
                format!("account root key is unusable: {e}"),
            )
            .await
        }
    };

    // Challenge: a fresh nonce, so a recorded handshake is worthless.
    let nonce = fresh_nonce()?;
    let challenge = HelloReply::Challenge {
        v: RELAY_VERSION,
        nonce: nonce.to_vec(),
    };
    write_frame(&mut send, &encode_message(&challenge)?).await?;

    // Auth: the certificate and the proof of possession.
    let body = read_frame(&mut recv, &mut buf, MAX_HANDSHAKE_BYTES).await?;
    let auth: Auth = decode_message(&body)?;
    let device = match verify_auth(&hello.account_id, &hello.device_id, &root, &nonce, &auth) {
        Ok(device) => device,
        Err(e) => {
            eprintln!(
                "arreo-relay: refused {peer} for account {}: {e}",
                hello.account_id
            );
            router.record(
                crate::audit::RelayAuditEvent::new(
                    crate::audit::actions::REFUSE,
                    arreo_core::store::AuditOutcome::Refused,
                )
                .account(hello.account_id.clone())
                .device(hello.device_id.clone())
                .peer(peer)
                .proto_version(hello.v)
                // The reason verbatim: it is the verifier's own sentence, and it
                // distinguishes a bad certificate from a bad proof of possession —
                // which are the same word ("auth") to a reader of the status line.
                .detail(e.to_string()),
            );
            return refuse_auth(&mut send, &connection, e.to_string()).await;
        }
    };

    // A device that completed a handshake is not an enumeration attempt: forget
    // its history, so a phone that reconnects after a real network drop is not
    // punished for the retries that got it here. Refusals deliberately do *not*
    // forgive — a peer that keeps failing is exactly who the budget is for.
    router.limiter.forgive(peer.ip());
    let session = Session {
        account_id: device.account_id.clone(),
        device_id: device.device_id.clone(),
        nonce: nonce.to_vec(),
        public_key: device.public_key,
    };
    router.store.touch_device(
        &session.account_id,
        session.device_id.as_str(),
        crate::directory::now_ms(),
    )?;
    write_frame(
        &mut send,
        &encode_message(&AuthReply::Welcome {
            v: RELAY_VERSION,
            account_id: session.account_id.clone(),
            device_id: session.device_id.display_id(),
        })?,
    )
    .await?;
    eprintln!(
        "arreo-relay: {peer} authenticated as {} in account {}",
        session.device_id, session.account_id
    );
    router.record(
        crate::audit::RelayAuditEvent::new(
            crate::audit::actions::SESSION_CONNECT,
            arreo_core::store::AuditOutcome::Ok,
        )
        .account(session.account_id.clone())
        .device(session.device_id.as_str().to_string())
        .peer(peer)
        .proto_version(hello.v),
    );

    // From here the connection is two independent directions: a writer task
    // owns the send half, and this task owns the read half.
    let (outbound, mut queue) = mpsc::channel::<Outbound>(OUTBOUND_QUEUE);
    let token = router.register(&session, outbound.clone());
    let writer_session = session.clone();
    let writer = tokio::spawn(async move {
        while let Some(item) = queue.recv().await {
            let frame = match item {
                Outbound::Envelope(envelope) => envelope.encode(),
                Outbound::Status { seq, outcome } => {
                    status_envelope(&writer_session, seq, &outcome).and_then(|e| e.encode())
                }
                Outbound::Drain(report) => {
                    drain_report_envelope(&writer_session, &report).and_then(|e| e.encode())
                }
                Outbound::PeerGone(departed) => {
                    peer_gone_envelope(&writer_session, &departed).and_then(|e| e.encode())
                }
                Outbound::Directory(reply) => {
                    directory_envelope(&writer_session, &reply).and_then(|e| e.encode())
                }
                // Already framed: writing it directly is what keeps the stored
                // bytes opaque end to end.
                Outbound::Raw(bytes) => Ok(bytes),
                Outbound::Displaced => {
                    // Returning drops `send`, which ends the stream: the session is
                    // over, and the device's client learns the way it learns about
                    // any other ending (T-0060).
                    eprintln!(
                        "arreo-relay: ending a displaced session for {} — a newer session \
                         holds the route",
                        writer_session.device_id
                    );
                    return;
                }
            };
            match frame {
                Ok(bytes) => {
                    if write_frame(&mut send, &bytes).await.is_err() {
                        return;
                    }
                }
                Err(e) => {
                    eprintln!("arreo-relay: cannot encode an outbound message: {e}");
                    return;
                }
            }
        }
    });

    // Read envelopes until the device goes away.
    let result = read_loop(&mut recv, &mut buf, &router, &session, &outbound).await;
    router.deregister(&session, token);
    // Presence truth (T-0031): the disconnect writes `last_seen` now, so
    // `online` cannot outlive the socket by more than the 90 s window. A relay
    // `kill -9` skips this line, and that is fine — presence is recomputed from
    // storage, so the dead device simply ages out instead of being cleared.
    let _ = router.store().touch_device(
        &session.account_id,
        session.device_id.as_str(),
        crate::directory::now_ms(),
    );
    drop(outbound);
    let _ = writer.await;
    eprintln!(
        "arreo-relay: {} disconnected ({})",
        session.device_id,
        if result.is_ok() { "clean" } else { "error" }
    );
    // A disconnect is `ok` either way: the outcome is about the *record*, and the
    // fact being recorded is that the session ended. Whether it ended cleanly is
    // the detail, where a reader can see it without it changing the vocabulary.
    router.record(
        crate::audit::RelayAuditEvent::new(
            crate::audit::actions::SESSION_DISCONNECT,
            arreo_core::store::AuditOutcome::Ok,
        )
        .account(session.account_id.clone())
        .device(session.device_id.as_str().to_string())
        .detail(if result.is_ok() { "clean" } else { "error" }),
    );
    result
}

/// The envelope read loop: validate, decide, deliver, report.
async fn read_loop<R>(
    recv: &mut R,
    buf: &mut Vec<u8>,
    router: &Arc<Router>,
    session: &Session,
    outbound: &mpsc::Sender<Outbound>,
) -> Result<(), RouterError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    loop {
        let envelope = match read_envelope(recv, buf).await {
            Ok(envelope) => envelope,
            // The peer closing is the normal end of a session, not an error to
            // shout about.
            Err(RelayError::Transport(_)) => return Ok(()),
            Err(e) => return Err(RouterError::Protocol(e)),
        };
        let seq = envelope.header.seq;

        // Control messages (T-0030) never reach the routing decision: they are
        // the device talking about its own inbox, not sending to a peer.
        match envelope.header.kind {
            RelayKind::Drain => {
                let request: DrainRequest = decode_payload(&envelope.payload)?;
                let drained = router.inbox.drain(
                    session.device_id.as_str(),
                    request.from_seq,
                    crate::inbox::DEFAULT_DRAIN_LIMIT,
                    crate::directory::now_ms(),
                )?;
                // The messages first, then the report: a device that stops
                // reading mid-batch sees messages without a report and drains
                // again, which is exactly the at-least-once contract.
                for raw in &drained.raw {
                    if outbound.try_send(Outbound::Raw(raw.clone())).is_err() {
                        eprintln!(
                            "arreo-relay: {} is not reading its drain; stopping this batch",
                            session.device_id
                        );
                        break;
                    }
                }
                let report = drain_report(&drained);
                let _ = outbound.try_send(Outbound::Drain(report));
                continue;
            }
            RelayKind::Ack => {
                let ack: Ack = decode_payload(&envelope.payload)?;
                router.inbox.ack(session.device_id.as_str(), ack.seq)?;
                continue;
            }
            // The machine directory (T-0056): the relay owns the only copy, so
            // these two are answered here rather than routed.
            RelayKind::Join => {
                let request: JoinRequest = decode_payload(&envelope.payload)?;
                let reply = router.join(session, &request, seq);
                let _ = outbound.try_send(Outbound::Directory(reply));
                continue;
            }
            RelayKind::Machines => {
                let request: MachinesRequest = decode_payload(&envelope.payload)?;
                let reply = router.machines(session, request.all, seq);
                let _ = outbound.try_send(Outbound::Directory(reply));
                continue;
            }
            // The directory's write side (T-0057). The relay applies the rules
            // the directory owns — never the client's idea of them.
            RelayKind::Rename => {
                let request: RenameRequest = decode_payload(&envelope.payload)?;
                let reply = router.rename(session, &request, seq);
                let _ = outbound.try_send(Outbound::Directory(reply));
                continue;
            }
            RelayKind::Remove => {
                let request: RemoveRequest = decode_payload(&envelope.payload)?;
                let reply = router.remove(session, &request, seq);
                let _ = outbound.try_send(Outbound::Directory(reply));
                continue;
            }
            RelayKind::Stale => {
                let request: StaleRequest = decode_payload(&envelope.payload)?;
                let reply = router.prune_stale(session, request, seq);
                let _ = outbound.try_send(Outbound::Directory(reply));
                continue;
            }
            RelayKind::Frame => {}
            // A device may not originate the relay's own kinds: a forged
            // departure notice would let any device in an account make another
            // device's peers drop their streams.
            RelayKind::Status | RelayKind::PeerGone | RelayKind::Directory => {
                let _ = outbound.try_send(Outbound::Status {
                    seq,
                    outcome: Outcome::Refused {
                        reason: "a device may not send status envelopes".to_string(),
                    },
                });
                continue;
            }
        }

        // The heartbeat (T-0031): every envelope a device sends refreshes its
        // `last_seen_ms`, so a connected device stays `online` without a new
        // wire kind — and a half-open connection whose peer went quiet ages out
        // on its own. One indexed write per envelope is the cost; a dedicated
        // heartbeat kind would add a second path for the same fact.
        let _ = router.store().touch_device(
            &session.account_id,
            session.device_id.as_str(),
            crate::directory::now_ms(),
        );

        // A frame addressed to self is not routed, it is consumed here. The
        // touch above already recorded it as `last_seen_ms`, and answering
        // `delivered` would be a lie — nothing was delivered, because there is
        // no peer. Consuming it here keeps one framing for "I am alive" without
        // creating a phantom peer on the sender's own session (its reader would
        // otherwise announce itself as a new peer and open a stream to itself).
        if envelope.header.dst == session.device_id.as_str()
            || envelope.header.dst == session.device_id.display_id()
        {
            let _ = outbound.try_send(Outbound::Status {
                seq,
                outcome: Outcome::Delivered,
            });
            continue;
        }

        let outcome = match router.decide(session, &envelope.header) {
            Decision::Deliver(destination) => {
                match destination.try_send(Outbound::Envelope(envelope)) {
                    Ok(()) => Outcome::Delivered,
                    // The destination is connected but not keeping up; that is the
                    // same answer as offline, and the honest one.
                    Err(_) => Outcome::Offline,
                }
            }
            Decision::Queue(dst) => {
                let body = envelope.encode()?;
                // The envelope is stored *whole*, exactly as it arrived: the
                // relay keeps the framing it must replay and never decodes the
                // header inside it, so the queue holds bytes rather than
                // understanding. The sender is told `queued` only after the
                // commit returns, which is the durability claim.
                match router
                    .inbox
                    .enqueue(dst.as_str(), &body, crate::directory::now_ms())
                {
                    Ok(enqueued) => Outcome::Queued {
                        queued: enqueued.queued,
                    },
                    Err(e) => {
                        eprintln!(
                            "arreo-relay: cannot queue for {dst}: {e}; refusing instead of \
                             dropping silently"
                        );
                        Outcome::Refused {
                            reason: format!("inbox refused the message: {e}"),
                        }
                    }
                }
            }
            Decision::NoSuchDevice => Outcome::NoSuchDevice,
            Decision::Refuse(reason) => {
                eprintln!(
                    "arreo-relay: refused envelope from {}: {reason}",
                    session.device_id
                );
                Outcome::Refused { reason }
            }
        };

        // Every envelope gets a report, so a sender never has to guess whether
        // its bytes went anywhere.
        if outbound
            .try_send(Outbound::Status { seq, outcome })
            .is_err()
        {
            eprintln!(
                "arreo-relay: {} is not reading its delivery reports; dropping one",
                session.device_id
            );
        }
    }
}

/// Finish a handshake with a refusal, and make sure the peer can read it.
///
/// Writing the frame is not enough: dropping the connection right afterwards
/// can discard it, and the peer then reports "connection lost" instead of the
/// reason the relay actually gave. So the stream is finished (which delivers
/// what was written) and the connection is left open briefly for the peer to
/// read and close — bounded, so a peer that never does cannot pin a task.
async fn refuse_hello<S>(
    send: &mut S,
    connection: &Connection,
    reason: String,
) -> Result<(), RouterError>
where
    S: tokio::io::AsyncWrite + Unpin,
{
    let reply = HelloReply::Refused {
        v: RELAY_VERSION,
        reason,
    };
    let _ = write_frame(send, &encode_message(&reply)?).await;
    // `shutdown` (not quinn's `finish`): the relay does not name the transport's
    // types, and for a stream the two mean the same thing.
    let _ = tokio::io::AsyncWriteExt::shutdown(send).await;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), connection.closed()).await;
    Ok(())
}

/// The same for the post-challenge stage, where the peer expects an [`AuthReply`].
async fn refuse_auth<S>(
    send: &mut S,
    connection: &Connection,
    reason: String,
) -> Result<(), RouterError>
where
    S: tokio::io::AsyncWrite + Unpin,
{
    let reply = AuthReply::Refused {
        v: RELAY_VERSION,
        reason,
    };
    let _ = write_frame(send, &encode_message(&reply)?).await;
    let _ = tokio::io::AsyncWriteExt::shutdown(send).await;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), connection.closed()).await;
    Ok(())
}

/// The report a drain is answered with.
fn drain_report(drained: &Drained) -> DrainReport {
    DrainReport {
        v: RELAY_VERSION,
        delivered: drained.messages.len() as u64,
        dropped: drained.dropped,
        expired: drained.expired,
        queued: drained.queued,
        next_seq: drained.next_seq,
    }
}

/// A drain report as the payload of a status envelope from the relay.
fn drain_report_envelope(
    session: &Session,
    report: &DrainReport,
) -> Result<RelayEnvelope, RelayError> {
    Ok(RelayEnvelope {
        header: RelayHeader {
            v: RELAY_VERSION,
            account_id: session.account_id.clone(),
            src_device: RELAY_SENDER.to_string(),
            dst: session.device_id.display_id(),
            seq: report.next_seq,
            kind: RelayKind::Status,
        },
        payload: encode_payload(report)?,
    })
}

/// A status envelope from the relay itself.
/// The departure notice for one device (T-0054).
///
/// The whole message is the header: `src_device` names the device that left, and
/// there is no payload because there is nothing else to say. The sender is the
/// relay's reserved word, so a receiver cannot mistake it for a device's frame.
/// Is `signature` by `key` over this session's join payload? Parsing the key and
/// the signature belongs to `arreo-core::identity::keys`, so the wire forms have
/// exactly one definition (T-0056).
fn verify_join_proof(
    key: &VerifyingKey,
    nonce: &[u8],
    account_id: &str,
    machine_key: &str,
    name: &str,
    signature: &[u8],
) -> bool {
    let payload = join_proof_payload(nonce, account_id, machine_key, name);
    arreo_core::identity::keys::verify_bytes(key, &payload, signature)
}

fn peer_gone_envelope(session: &Session, departed: &DeviceId) -> Result<RelayEnvelope, RelayError> {
    Ok(RelayEnvelope {
        header: RelayHeader {
            v: RELAY_VERSION,
            account_id: session.account_id.clone(),
            src_device: departed.as_str().to_string(),
            dst: session.device_id.display_id(),
            seq: 0,
            kind: RelayKind::PeerGone,
        },
        payload: Vec::new(),
    })
}

/// A directory refusal: the answer to a request the relay would not perform.
fn refuse(seq: u64, reason: String) -> DirectoryReply {
    DirectoryReply {
        v: RELAY_VERSION,
        seq,
        granted: None,
        machines: Vec::new(),
        refused: Some(reason),
    }
}

/// The always-present framing of the relay's own answers: the relay as sender,
/// this device as destination, and the request's sequence number so the client
/// can route the answer (T-0056).
fn directory_envelope(
    session: &Session,
    reply: &DirectoryReply,
) -> Result<RelayEnvelope, RelayError> {
    Ok(RelayEnvelope {
        header: RelayHeader {
            v: RELAY_VERSION,
            account_id: session.account_id.clone(),
            src_device: RELAY_SENDER.to_string(),
            dst: session.device_id.display_id(),
            seq: reply.seq,
            kind: RelayKind::Directory,
        },
        // The version travels in the payload as well as the header, so a reply
        // is self-describing even when lifted out of its envelope (the export
        // path does exactly that).
        payload: encode_payload(reply)?,
    })
}

fn status_envelope(
    session: &Session,
    seq: u64,
    outcome: &Outcome,
) -> Result<RelayEnvelope, RelayError> {
    Ok(RelayEnvelope {
        header: RelayHeader {
            v: RELAY_VERSION,
            account_id: session.account_id.clone(),
            src_device: RELAY_SENDER.to_string(),
            dst: session.device_id.display_id(),
            seq,
            kind: RelayKind::Status,
        },
        payload: encode_payload(outcome)?,
    })
}

/// A human-readable one-liner for the operator's log when the relay starts.
#[must_use]
pub fn describe_listen(addr: SocketAddr) -> String {
    if addr.ip().is_loopback() {
        format!("loopback only ({addr})")
    } else {
        format!(
            "{addr} — reachable off this machine: the relay authenticates each device by \
             verifying its certificate against the account's registered root key, and never \
             reads what it routes, but anyone who can reach this port can open a session and be \
             refused"
        )
    }
}
