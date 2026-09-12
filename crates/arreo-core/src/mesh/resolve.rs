//! A machine name, resolved through the account's directory to a dialable target
//! (T-0045, ROADMAP §3.7).
//!
//! One sentence: the name picks a row, the row's `daemon_key` is *which key to
//! dial*, and the result is the same [`Target`] a local attach builds — so
//! "reach another machine by name" is a resolution step, not a second protocol.
//!
//! ## Why this lives in `arreo-core` and not in a client
//!
//! Two clients resolve names now — `arreo attach` and `arreo-tui` — and a second
//! copy of this is a second opinion about what a name means, what an offline
//! machine should cost, and which failure is which. `arreo-tui` may not depend on
//! `arreo-cli` (a binary), so the shared home is here, beside the client it feeds.
//! This is the same move T-0045 made for [`crate::mesh::session`] itself: the
//! implementation follows the number of callers.
//!
//! The error is a **typed** failure rather than an exit code, because that is what
//! the callers need to agree on: the CLI maps kinds to its exit-code vocabulary
//! (`arreo machines`' one vocabulary), the TUI maps them to one message on stderr.
//! A `u8` here would have made this module's contract one consumer's convention.
//!
//! ## What a name has to resolve to, and why
//!
//! The directory is keyed by **machine id**, which is a machine's *root* key
//! (`MachineId::from_key`, T-0043) — an identity, not a route. Reaching a machine
//! needs the key its **daemon** authenticates with, because:
//!
//! - the relay moves bytes between **device ids**, and a machine's daemon
//!   authenticates as a device (the one pairing created for it) — a different key
//!   from the machine's root key;
//! - the Noise handshake proves the peer **holds the key we pinned** (ADR 0011),
//!   so a client needs the key itself: an id cannot be turned into one.
//!
//! Both come from the row's `daemon_key` (T-0045), which the relay writes from the
//! certificate that authenticated the session asserting the row — a machine cannot
//! advertise a route it does not hold. The alternative, letting the relay say who
//! is at the other end, is what ADR 0011 forbids.

use super::session::{RemoteTarget, Target};
use super::Presence;
use crate::identity::{
    self, verifying_key_from_hex, DeviceCert, DeviceId, DeviceKey, VerifyingKey,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Why a name did not resolve to something dialable.
///
/// The three kinds are the three things a caller must do about it: fix its own
/// invocation, correct the name, or wait/repair the other machine. A single
/// "resolve failed" string would push that classification back onto every caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// The caller's own configuration: no `--config`, or an unreadable one.
    Usage(String),
    /// No such machine in this account (or its name is held by a tombstone).
    UnknownMachine(String),
    /// The machine is known but cannot be dialed right now — the relay is
    /// unreachable, the row has no dial key yet, or the directory says it is not
    /// online.
    Unreachable(String),
}

impl ResolveError {
    /// The message, for a caller that has its own label to put in front.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::Usage(message) | Self::UnknownMachine(message) | Self::Unreachable(message) => {
                message
            }
        }
    }
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for ResolveError {}

/// A machine resolved through the directory: where to dial, and what the
/// directory said about it.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub target: Target,
    /// The name the directory actually holds — a machine renamed since the
    /// operator last looked is reported as it is now, not as they typed it.
    pub name: String,
    pub presence: Presence,
    pub last_seen_ms: i64,
}

impl Resolved {
    /// The directory's view, for a message after a failure. T-0045 asks for it:
    /// "no answer" is only half the story, and how stale the machine is decides
    /// what the operator does next.
    ///
    /// **The absolute timestamp, not a computed age.** `last_seen_ms` is on the
    /// *relay's* clock — it is the relay that observed the heartbeat — so an age
    /// computed here would silently mix two clocks, and the interesting case is
    /// exactly the one where they differ (a relay restarted, or a machine whose
    /// clock is off). The timestamp is the relay's own statement; how long ago that
    /// was is arithmetic the reader can do against a clock they trust.
    #[must_use]
    pub fn directory_note(&self) -> String {
        format!(
            "directory says {} (last seen {}); tried relay",
            self.presence.as_str(),
            crate::store::rfc3339_ms(self.last_seen_ms)
        )
    }
}

/// How this machine reaches the account: the relay and account from `[relay]`.
struct Account {
    relay: std::net::SocketAddr,
    account: String,
}

/// Read this machine's `[relay]` configuration, or say what to do about it.
///
/// One parser for the whole product (`crate::relay::config`): the daemon,
/// `arreo machines` and this verb must agree about what a valid section is.
fn account(config: Option<&Path>) -> Result<Account, ResolveError> {
    let path = match config
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("ARREO_CONFIG").map(PathBuf::from))
    {
        Some(path) => path,
        None => {
            return Err(ResolveError::Usage(
                "which account am I in? pass --config PATH or set ARREO_CONFIG (its [relay] \
                 section names the relay and the account)"
                    .to_string(),
            ))
        }
    };
    match crate::relay::config::load_config(&path) {
        Ok(Some(settings)) => Ok(Account {
            relay: settings.addr,
            account: settings.account,
        }),
        Ok(None) => Err(ResolveError::Unreachable(format!(
            "no relay is configured ({}), so another machine cannot be reached by name",
            path.display()
        ))),
        Err(e) => Err(ResolveError::Usage(e.to_string())),
    }
}

/// This machine's own device identity: the key and certificate `arreo pair`
/// saved, which is what the peer's trust ledger sees.
fn own_device() -> Result<(DeviceKey, DeviceCert), ResolveError> {
    let dir = identity::identity_root();
    let key_path = dir.join("device.key");
    let key = DeviceKey::load(&key_path).map_err(|e| {
        ResolveError::Unreachable(format!(
            "this machine has no paired identity ({}): {e}",
            key_path.display()
        ))
    })?;
    let id = DeviceId::from_key(&key.public());
    let cert_path = dir.join("devices").join(format!("{}.cert", id.as_str()));
    let cert = DeviceCert::load(&cert_path).map_err(|e| {
        ResolveError::Unreachable(format!("no certificate at {}: {e}", cert_path.display()))
    })?;
    Ok((key, cert))
}

/// Resolve a machine name to a dialable target through the directory.
pub async fn by_name(name: &str, config: Option<&Path>) -> Result<Resolved, ResolveError> {
    let account = account(config)?;
    let (key, cert) = own_device()?;

    // Ask the relay — the account's directory, not a local cache. A name only
    // exists in the account, and an address remembered from a previous session is
    // exactly what the directory exists to avoid.
    let session =
        crate::relay::session::RelaySession::dial(account.relay, &account.account, &key, &cert)
            .await
            .map_err(|e| {
                ResolveError::Unreachable(format!(
                    "cannot reach the relay at {}: {e}",
                    account.relay
                ))
            })?;
    let reply = session.machines(true).await.map_err(|e| {
        ResolveError::Unreachable(format!(
            "the relay did not answer the directory request: {e}"
        ))
    })?;
    if let Some(reason) = reply.refused {
        return Err(ResolveError::Unreachable(format!(
            "the relay refused: {reason}"
        )));
    }

    let Some(row) = reply.machines.iter().find(|row| row.name.as_str() == name) else {
        let known: Vec<&str> = reply
            .machines
            .iter()
            .filter(|row| row.tombstone_until_ms.is_none())
            .map(|row| row.name.as_str())
            .collect();
        return Err(ResolveError::UnknownMachine(match known.is_empty() {
            true => format!("no machine named {name:?}: this account lists no machines"),
            false => format!("no machine named {name:?} in this account (have: {known:?})"),
        }));
    };
    if let Some(until) = row.tombstone_until_ms {
        return Err(ResolveError::UnknownMachine(format!(
            "{name:?} was removed from this account and its name is held until {}",
            crate::store::rfc3339_ms(until)
        )));
    }

    // Present means routable. Absent means the row predates the dial key, and the
    // fix is on the machine itself: its daemon asserts the row on every relay
    // connect.
    let Some(daemon_key_hex) = row.daemon_key.clone() else {
        return Err(ResolveError::Unreachable(format!(
            "{name:?} is in the account but has published no dial key yet (its row predates \
             it). Once its daemon has connected to the relay, try again"
        )));
    };
    let server_key: VerifyingKey = verifying_key_from_hex(&daemon_key_hex).map_err(|e| {
        ResolveError::Unreachable(format!("{name:?} published an unusable dial key: {e}"))
    })?;
    // **Ask the directory before dialing (T-0045's "fast and honest").** The relay
    // already knows whether this machine has been seen recently, so a machine that
    // is plainly offline costs one directory round trip instead of a dial plus a
    // handshake timeout — and the operator is told *why* rather than waiting to
    // find out. The dial below remains the backstop for the case the directory
    // cannot see: a machine that went away between its last heartbeat and now.
    if row.presence != Presence::Online {
        let seen = crate::store::rfc3339_ms(row.last_seen_ms);
        return Err(ResolveError::Unreachable(format!(
            "{name:?} is {} (last seen {seen}), so nothing was dialed. Its daemon \
             will reappear in the directory when it reconnects",
            row.presence.as_str(),
        )));
    }

    let peer = DeviceId::from_key(&server_key);
    Ok(Resolved {
        name: row.name.as_str().to_string(),
        presence: row.presence,
        last_seen_ms: row.last_seen_ms,
        target: Target::Remote(Box::new(RemoteTarget {
            relay: account.relay,
            account: account.account,
            peer,
            server_key,
            device: Arc::new(key),
            cert: Arc::new(cert),
        })),
    })
}
