//! Pairing, both sides (T-0104).
//!
//! One sentence: `pairing_server_begin` shows a code and waits; the phone parses
//! the invite URI, joins with the code, and gets back the certificate — and both
//! halves are the same state machines the CLI drives, so the phone pairs with
//! the same mailbox the CLI pairs with.
//!
//! **Everything here is blocking.** The core's pairing flow is synchronous
//! (`std::net` / `std::os::unix::net` with a ten-second read timeout), the CLI
//! drives it from a sync function, and the TUI wraps it in `spawn_blocking`.
//! UniFFI has no way to say "this call parks the calling thread", so the honest
//! shape is a synchronous export plus a docstring that says so: a mobile UI must
//! call `pairing_server_receive` and `pairing_phone_await_cert` off its main
//! thread. Making them `async` here would only move the blocking behind a future
//! that never yields.
//!
//! **The device key arrives as a seed** (see [`crate::identity`]) because
//! `PairingPhone::join` consumes the keypair and hands it back inside the
//! `PairedDevice` — "a stored certificate without its key is a device that can
//! never authenticate". Taking a seed means the phone's keystore keeps owning
//! the secret and this crate never has to hand one back across the boundary.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use arreo_core::identity::DeviceKey;
use arreo_core::pairing::flow::{DirectoryHint, Invite, PairingPhone, PairingServer};
use arreo_core::pairing::{Code, MailboxAddr, PhoneRequest};

use crate::errors::{bad_seed, PairingFfiError};
use crate::identity::{DeviceCertHandle, FfiRole, RootKeyHandle};

/// The parsed invite: what the QR carries, in typed form.
///
/// `account` and `relay` are present only when the admitting machine belongs to
/// an account (T-0058) — they are what a *joining machine* needs to register
/// itself, and they are public metadata, never a key.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct PairingInvite {
    /// The mailbox session id.
    pub session: String,
    /// The mailbox address: a unix socket path, or `host:port`.
    pub mailbox: String,
    /// Hex of the admitting machine's identity public key — what the phone pins.
    pub server_key: String,
    /// How long the mailbox session stays open, in seconds.
    pub ttl_secs: u64,
    /// The account the joining machine should register itself with, when the
    /// invite names one.
    pub account: Option<String>,
    /// The relay address the joining machine should register with, when the
    /// invite names one.
    pub relay: Option<String>,
}

impl PairingInvite {
    fn from_core(invite: &Invite) -> Self {
        Self {
            session: invite.session.clone(),
            mailbox: invite.mailbox.as_str(),
            server_key: invite.server_key.clone(),
            ttl_secs: invite.ttl.as_secs(),
            account: invite.directory.as_ref().map(|d| d.account.clone()),
            relay: invite.directory.as_ref().map(|d| d.relay.clone()),
        }
    }

    /// Back to the core's invite.
    ///
    /// A half-filled directory hint is refused with the core's own sentence for
    /// it: "both or neither", because a machine sent looking for an account on a
    /// relay it was never told about is the failure that rule exists to prevent.
    fn to_core(&self) -> Result<Invite, PairingFfiError> {
        let directory = match (&self.account, &self.relay) {
            (Some(account), Some(relay)) => Some(DirectoryHint {
                account: account.clone(),
                relay: relay.clone(),
            }),
            (None, None) => None,
            _ => {
                return Err(PairingFfiError::BadInvite(
                    "malformed invite: the invite names only one of the account and the relay"
                        .to_string(),
                ))
            }
        };
        Ok(Invite {
            session: self.session.clone(),
            mailbox: MailboxAddr::parse(&self.mailbox)?,
            server_key: self.server_key.clone(),
            ttl: Duration::from_secs(self.ttl_secs.max(1)),
            directory,
        })
    }
}

/// What the phone asked for, once the code verified.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct PairingRequest {
    /// The phone's public key, 64 lowercase hex — what the certificate is issued
    /// for.
    pub public_key: String,
    /// The device's label ("pixel-7"). Display only, and this boundary derives
    /// nothing from it.
    pub name: String,
}

/// What a successful pairing hands the caller.
///
/// The certificate and the fingerprint together, so a UI can show "paired as
/// `dev_…`" from one value and store the other. The keypair is not here: the
/// caller supplied its seed and still holds it, which is the whole point of the
/// seed-shaped boundary. The certificate's bytes come from
/// `DeviceCertHandle::encode`, so there is one spelling of them.
#[derive(uniffi::Record)]
pub struct PairedDeviceInfo {
    /// The certificate, ready to be used as a relay session credential.
    pub cert: Arc<DeviceCertHandle>,
    /// The device's identity in the display spelling: `dev_<hex>`.
    pub device_id: String,
    /// The device's fingerprint: the bare 32-hex identity.
    pub fingerprint: String,
    /// The device's label, as the certificate carries it.
    pub name: String,
    /// The role the admitting machine granted.
    pub role: FfiRole,
}

/// Parse an invite URI (what a QR encodes).
#[uniffi::export]
pub fn pairing_invite_parse(uri: String) -> Result<PairingInvite, PairingFfiError> {
    Ok(PairingInvite::from_core(&Invite::parse_uri(&uri)?))
}

/// Render an invite back to its URI — what the admitting side draws as a QR.
#[uniffi::export]
pub fn pairing_invite_uri(invite: PairingInvite) -> Result<String, PairingFfiError> {
    Ok(invite.to_core()?.uri())
}

/// A fresh pairing code, as the four words a human reads out.
#[uniffi::export]
pub fn pairing_code_random() -> Result<String, PairingFfiError> {
    Ok(Code::random()?.phrase())
}

/// Canonicalize a typed code. A wrong word is refused with the core's hint
/// ("did you mean …?"), which is the sentence the CLI shows.
#[uniffi::export]
pub fn pairing_code_phrase(text: String) -> Result<String, PairingFfiError> {
    Ok(Code::parse(&text)?.phrase())
}

/// The admitting half: it holds the code, and it is the side that issues.
#[derive(uniffi::Object)]
pub struct PairingServerHandle {
    /// `Option` because `receive` takes `&mut self` and `complete`/`abandon`
    /// consume it: the core's state machine is a linear one, and the boundary
    /// keeps that shape rather than pretending the server is reusable.
    server: Mutex<Option<PairingServer>>,
}

/// Begin a pairing: pick a session id and a code, and publish the first flight.
///
/// `mailbox` is a unix socket path or `host:port` (the two shapes
/// `MailboxAddr::parse` accepts); `account`/`relay` are the directory hint a
/// *joining machine* needs, or `None` for an ordinary pairing.
///
/// Blocking: it opens the mailbox session and writes flight A.
#[uniffi::export]
pub fn pairing_server_begin(
    server_identity: Arc<RootKeyHandle>,
    mailbox: String,
    ttl_secs: u64,
    account: Option<String>,
    relay: Option<String>,
) -> Result<Arc<PairingServerHandle>, PairingFfiError> {
    let directory = match (account, relay) {
        (Some(account), Some(relay)) => Some(DirectoryHint { account, relay }),
        (None, None) => None,
        _ => {
            return Err(PairingFfiError::BadInvite(
                "malformed invite: the invite names only one of the account and the relay"
                    .to_string(),
            ))
        }
    };
    let addr = MailboxAddr::parse(&mailbox)?;
    let server = PairingServer::begin(
        server_identity.inner(),
        addr,
        Duration::from_secs(ttl_secs.max(1)),
        directory,
    )?;
    Ok(Arc::new(PairingServerHandle {
        server: Mutex::new(Some(server)),
    }))
}

#[uniffi::export]
impl PairingServerHandle {
    /// The invite to display (and to draw as a QR).
    pub fn invite(&self) -> Result<PairingInvite, PairingFfiError> {
        let guard = self.lock()?;
        let server = guard.as_ref().ok_or_else(|| spent("invite"))?;
        Ok(PairingInvite::from_core(server.invite()))
    }

    /// The code to read out: four words, the only shared secret.
    pub fn code(&self) -> Result<String, PairingFfiError> {
        let guard = self.lock()?;
        let server = guard.as_ref().ok_or_else(|| spent("code"))?;
        Ok(server.code().phrase())
    }

    /// Wait for the phone's hello and confirm it.
    ///
    /// **Blocking**, for up to the pairing TTL. A wrong code burns the session
    /// and comes back as `CodeMismatch` — the guess budget is spent, and the
    /// caller must not issue anything.
    pub fn receive(&self) -> Result<PairingRequest, PairingFfiError> {
        let mut guard = self.lock()?;
        let server = guard.as_mut().ok_or_else(|| spent("receive"))?;
        let request: PhoneRequest = server.receive()?;
        Ok(PairingRequest {
            public_key: hex(&request.public_key.to_bytes()),
            name: request.name,
        })
    }

    /// Send the certificate back. Takes the server by value: it is the only
    /// thing this side produces, and it must not be sent twice.
    pub fn complete(&self, cert: Arc<DeviceCertHandle>) -> Result<(), PairingFfiError> {
        Ok(self.take("complete")?.complete(cert.cert_ref())?)
    }

    /// Abandon the pairing explicitly (a cancelled scan, a rejected phone):
    /// burn the session so the code dies with it.
    pub fn abandon(&self) -> Result<(), PairingFfiError> {
        self.take("abandon")?.abandon();
        Ok(())
    }
}

impl PairingServerHandle {
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Option<PairingServer>>, PairingFfiError> {
        self.server
            .lock()
            .map_err(|_| PairingFfiError::Confirmation(poisoned()))
    }

    fn take(&self, verb: &str) -> Result<PairingServer, PairingFfiError> {
        let mut guard = self.lock()?;
        guard.take().ok_or_else(|| spent(verb))
    }
}

/// The joining half: it holds the code and the pinned server key.
#[derive(uniffi::Object)]
pub struct PairingPhoneHandle {
    phone: Mutex<Option<PairingPhone>>,
}

/// Join a pairing: read flight A, publish flight B and the hello.
///
/// `seed` is the device keypair's secret (32 bytes): the key is built from it
/// here and stays in memory until the certificate verifies, so a failed pairing
/// writes nothing anywhere. `name` is the label the admitting machine records.
///
/// Blocking: it waits for the server's first flight.
#[uniffi::export]
pub fn pairing_phone_join(
    invite_uri: String,
    code: String,
    seed: Vec<u8>,
    name: String,
) -> Result<Arc<PairingPhoneHandle>, PairingFfiError> {
    let invite = Invite::parse_uri(&invite_uri)?;
    let code = Code::parse(&code)?;
    let key = DeviceKey::from_seed(seed_array(&seed)?);
    let phone = PairingPhone::join(&invite, &code, key, &name)?;
    Ok(Arc::new(PairingPhoneHandle {
        phone: Mutex::new(Some(phone)),
    }))
}

#[uniffi::export]
impl PairingPhoneHandle {
    /// Wait for the server's reply and verify the certificate against the
    /// **pinned** server key from the invite.
    ///
    /// **Blocking**, for up to the invite's TTL.
    pub fn await_cert(&self) -> Result<PairedDeviceInfo, PairingFfiError> {
        let phone = self
            .phone
            .lock()
            .map_err(|_| PairingFfiError::Confirmation(poisoned()))?
            .take()
            .ok_or_else(|| spent("await_cert"))?;
        let paired = phone.await_cert()?;
        Ok(PairedDeviceInfo {
            device_id: paired.cert.device().display_id(),
            fingerprint: paired.cert.device().as_str().to_string(),
            name: paired.cert.name().to_string(),
            role: FfiRole::from(paired.cert.role()),
            cert: Arc::new(DeviceCertHandle::from_core(paired.cert)),
        })
    }
}

/// The refusal for a pairing that has already finished: the state machine is
/// linear, and this is what "you already used it" says.
fn spent(verb: &str) -> PairingFfiError {
    PairingFfiError::Confirmation(format!(
        "this pairing is already finished; {verb} needs a live session"
    ))
}

fn poisoned() -> String {
    "the pairing state is poisoned by a panicking call".to_string()
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// The boundary's seed check, with the one sentence [`crate::errors::bad_seed`]
/// owns.
fn seed_array(seed: &[u8]) -> Result<[u8; 32], PairingFfiError> {
    <[u8; 32]>::try_from(seed).map_err(|_| PairingFfiError::BadSeed(bad_seed(seed.len())))
}
