//! The two sides of a pairing (T-0024).
//!
//! One sentence: SPAKE2 turns the code into a shared key, a MAC over that key
//! confirms both sides guessed the same code, and only then does the server
//! issue a certificate — so a wrong guess leaves nothing behind except a burned
//! session.
//!
//! Sequence (slots are the mailbox's, see [`super::wire`]):
//!
//! ```text
//!   server                                    phone
//!   --------------------------------------    ------------------------------------
//!   code = 4 words (shown to the human)
//!   (A_state, msg_a) = SPAKE2.start_a(code)
//!   put(a, msg_a)                       --->  get(a)
//!   get(b)                              <---  (B_state, msg_b) = start_b(code); put(b, msg_b)
//!   K = A_state.finish(msg_b)                 K = B_state.finish(msg_a)
//!   get(c)                              <---  put(c, PhoneHello{key,name} + MAC_K)
//!   # MAC says "same code", not "same key"
//!   cert = authority.issue(phone key)         get(d)
//!   put(d, cert_bytes + MAC_K)          --->  verify cert against the pinned server key
//!   burn(session)                             save key + cert, burn(session)
//! ```
//!
//! **One guess.** A MAC that does not verify burns the session immediately
//! (server side) and aborts (phone side). An attacker who can read *and inject*
//! every flight gets exactly one attempt per session, and a session id is
//! single-use at the relay — so the 32-bit code is not brute-forceable in any
//! window a human would leave open.
//!
//! **What the exchange does not need.** No TLS: the flights are public by
//! design (a mailbox is a bulletin board), the code authenticates the exchange,
//! and the certificate that comes back is signed by the server root the phone
//! already pinned from the invite. A network attacker's usable move is
//! publishing a forged flight early, which fails confirmation — a denial of
//! service, never an impersonation.

use ed25519_dalek::{Signature, VerifyingKey};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use spake2::{Ed25519Group, Identity, Password, Spake2};
use std::time::{Duration, Instant};

use crate::identity::{DeviceCert, DeviceId, DeviceKey, RootKey};
use crate::pairing::code::Code;
use crate::pairing::wire::{MailboxAddr, MailboxClient, Slot};
use crate::pairing::PairingError;

/// How often a waiting side re-asks the mailbox. Short: pairing is interactive
/// and a human is watching, so latency is more visible than load (the mailbox
/// is two clients exchanging four small frames).
pub const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Domain-separation labels. Distinct labels stop a phone's flight from being
/// replayed as a server's.
const LABEL_PHONE: &[u8] = b"arreo/pair/v1/phone";
const LABEL_SERVER: &[u8] = b"arreo/pair/v1/server";
const MAC_KEY_LABEL: &[u8] = b"arreo/pair/v1/mac";
const ID_LABEL_SERVER: &str = "arreo-server";
const ID_LABEL_SESSION: &str = "arreo-session";

/// The default pairing window (§3.3: long enough to walk to the other device,
/// short enough that a forgotten terminal is not a standing invitation).
pub const DEFAULT_TTL: Duration = Duration::from_secs(300);

/// How much longer than the visible window the mailbox keeps a session.
///
/// The *displayed* window is the server's (and the phone's) deadline, and it is
/// what a human is told. The mailbox is storage: it must outlive the window,
/// or the relay's expiry can beat the server's own deadline and the user sees a
/// storage message ("session is no longer available") instead of "the pairing
/// window closed" — which is exactly what happened the first time. The holdover
/// also gives the last flight a place to land; the session is retired when the
/// relay's copy expires.
pub const MAILBOX_GRACE: Duration = Duration::from_secs(10);

/// What the server shows (and what the QR encodes): where the mailbox is, which
/// session, and *which server* — the phone pins that key, so the code alone is
/// never enough to make it trust a different machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invite {
    pub session: String,
    pub mailbox: MailboxAddr,
    /// Hex of the server's identity public key (its root key).
    pub server_key: String,
    pub ttl: Duration,
}

impl Invite {
    /// The URI a QR carries: `arreo://pair?v=1&mb=…&s=…&k=…&ttl=…`.
    ///
    /// The code is deliberately **not** in the URI: it is typed by a human on
    /// purpose, and a URI that contained it would turn "scan this" into "trust
    /// anything that renders a QR".
    #[must_use]
    pub fn uri(&self) -> String {
        format!(
            "arreo://pair?v=1&mb={}&s={}&k={}&ttl={}",
            percent_encode(&self.mailbox.as_str()),
            percent_encode(&self.session),
            percent_encode(&self.server_key),
            self.ttl.as_secs()
        )
    }

    /// Parse an invite URI. Strict: a missing or malformed field is refused
    /// rather than defaulted (a defaulted mailbox would silently pair against
    /// the wrong machine).
    pub fn parse_uri(uri: &str) -> Result<Self, PairingError> {
        let rest = uri
            .strip_prefix("arreo://pair?")
            .ok_or_else(|| PairingError::BadInvite(format!("not an arreo pairing URI: {uri:?}")))?;
        let mut fields = std::collections::HashMap::new();
        for pair in rest.split('&') {
            let (key, value) = pair.split_once('=').ok_or_else(|| {
                PairingError::BadInvite(format!("malformed field {pair:?} in the invite"))
            })?;
            fields.insert(key.to_string(), percent_decode(value)?);
        }
        let version = fields
            .get("v")
            .ok_or_else(|| PairingError::BadInvite("invite has no version".into()))?;
        if version != "1" {
            return Err(PairingError::BadInvite(format!(
                "unsupported invite version {version:?} (this build speaks 1)"
            )));
        }
        let mailbox = MailboxAddr::parse(
            fields
                .get("mb")
                .ok_or_else(|| PairingError::BadInvite("invite has no mailbox".into()))?,
        )?;
        let session = fields
            .get("s")
            .ok_or_else(|| PairingError::BadInvite("invite has no session id".into()))?
            .clone();
        let server_key = fields
            .get("k")
            .ok_or_else(|| PairingError::BadInvite("invite has no server key".into()))?
            .clone();
        let ttl_secs = fields
            .get("ttl")
            .map(|raw| {
                raw.parse::<u64>()
                    .map_err(|_| PairingError::BadInvite(format!("bad ttl {raw:?}")))
            })
            .transpose()?
            .unwrap_or(DEFAULT_TTL.as_secs());
        Ok(Self {
            session,
            mailbox,
            server_key,
            ttl: Duration::from_secs(ttl_secs.max(1)),
        })
    }

    /// The server's pinned public key.
    pub fn server_public_key(&self) -> Result<VerifyingKey, PairingError> {
        parse_public_key(&self.server_key)
    }
}

/// What a successful pairing hands the caller: the certificate **and** the
/// keypair it belongs to, together, so no caller can persist one without the
/// other (a stored certificate without its key is a device that can never
/// authenticate, and a stored key without its certificate is a device the
/// server will refuse).
pub struct PairedDevice {
    pub cert: DeviceCert,
    pub key: DeviceKey,
}

impl std::fmt::Debug for PairedDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairedDevice")
            .field("device", &self.cert.device())
            .field("role", &self.cert.role())
            .finish_non_exhaustive()
    }
}

impl PairedDevice {
    #[must_use]
    pub fn device_id(&self) -> &DeviceId {
        self.cert.device()
    }
}

/// What the server learned about the phone once the MAC verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhoneRequest {
    pub public_key: VerifyingKey,
    pub name: String,
}

/// The server half: it holds the code, and it is the side that issues.
pub struct PairingServer {
    invite: Invite,
    code: Code,
    mailbox: MailboxClient,
    /// SPAKE2 state, kept between publishing flight A and receiving flight B.
    state: Option<Spake2<Ed25519Group>>,
    /// Derived once flight B arrives; a wrong code shows up here.
    mac_key: Option<[u8; 32]>,
    deadline: Instant,
}

impl std::fmt::Debug for PairingServer {
    /// Never print the code or the derived key — a pairing is a secret in
    /// progress, and this is the side that lives on a server.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingServer")
            .field("session", &self.invite.session)
            .field("ttl", &self.invite.ttl)
            .field("confirmed", &self.mac_key.is_some())
            .finish_non_exhaustive()
    }
}

impl PairingServer {
    /// Begin a pairing: pick a session id and a code, and publish flight A.
    ///
    /// `server_identity` is the server's public key — the same key that signs
    /// device certificates, so the code authenticates the machine the phone is
    /// about to trust.
    pub fn begin(
        server_identity: &RootKey,
        mailbox: MailboxAddr,
        ttl: Duration,
    ) -> Result<Self, PairingError> {
        let session = random_session()?;
        let code = Code::random()?;
        let invite = Invite {
            session,
            mailbox,
            server_key: server_identity.public_hex(),
            ttl,
        };
        let mailbox_client = MailboxClient::new(invite.mailbox.clone());
        mailbox_client.open(&invite.session, ttl + MAILBOX_GRACE)?;

        let (state, flight_a) = Spake2::<Ed25519Group>::start_a(
            &Password::new(code.password()),
            &identity_a(&invite),
            &identity_b(&invite),
        );
        mailbox_client.put(&invite.session, Slot::A, &flight_a)?;

        Ok(Self {
            invite,
            code,
            mailbox: mailbox_client,
            state: Some(state),
            mac_key: None,
            deadline: Instant::now() + ttl,
        })
    }

    /// The invite to display, and the code to read out.
    #[must_use]
    pub fn invite(&self) -> &Invite {
        &self.invite
    }

    #[must_use]
    pub fn code(&self) -> &Code {
        &self.code
    }

    /// Wait for the phone's hello and confirm it. A MAC that does not verify
    /// burns the session and returns [`PairingError::CodeMismatch`]: the guess
    /// budget is spent, and the caller must not issue anything.
    pub fn receive(&mut self) -> Result<PhoneRequest, PairingError> {
        let flight_b =
            self.mailbox
                .wait_for(&self.invite.session, Slot::B, self.deadline, POLL_INTERVAL)?;
        let state = self.state.take().ok_or(PairingError::Confirmation)?;
        let shared = state
            .finish(&flight_b)
            .map_err(|e| PairingError::Spake(e.to_string()))?;
        let mac_key = derive_mac_key(&shared);
        self.mac_key = Some(mac_key);

        let hello =
            self.mailbox
                .wait_for(&self.invite.session, Slot::C, self.deadline, POLL_INTERVAL)?;
        let (body, tag) = split_mac(&hello, LABEL_PHONE, &self.invite.session)?;
        if !verify_mac(&mac_key, LABEL_PHONE, &self.invite.session, body, &tag) {
            // One guess: burn it so an enumeration attempt costs the attacker
            // the session, and so the human sees "wrong code", not a hang.
            let _ = self.mailbox.burn(&self.invite.session);
            return Err(PairingError::CodeMismatch);
        }
        let hello: PhoneHello = rmp_serde::from_slice(body)
            .map_err(|e| PairingError::Mailbox(format!("malformed phone hello: {e}")))?;
        Ok(PhoneRequest {
            public_key: parse_public_key(&hello.public_key)
                .map_err(|e| PairingError::Identity(e.to_string()))?,
            name: hello.name,
        })
    }

    /// Send the certificate back. Takes the certificate by value: it is the
    /// only thing this side produces, and it must not be sent twice.
    ///
    /// **The server does not burn here.** Burning drops the payloads, and the
    /// phone has not read slot D yet — the first version of this function
    /// burned after publishing, and the phone's very next poll got "session is
    /// no longer available" while its certificate sat unread (found by the
    /// real-process test, which is exactly what it is for). Retirement moves to
    /// whoever is last: the phone burns after it verifies, and a phone that
    /// never arrives is covered by the TTL, which retires the id at expiry.
    pub fn complete(self, cert: &DeviceCert) -> Result<(), PairingError> {
        let mac_key = self.mac_key.ok_or(PairingError::Confirmation)?;
        let bytes = cert
            .encode()
            .map_err(|e| PairingError::Mailbox(format!("cannot encode the certificate: {e}")))?;
        let framed = frame_mac(&mac_key, LABEL_SERVER, &self.invite.session, &bytes);
        self.mailbox.put(&self.invite.session, Slot::D, &framed)
    }

    /// Abandon the pairing explicitly (a Ctrl-C, or a rejected phone): burn the
    /// session so the code dies with it.
    pub fn abandon(self) {
        let _ = self.mailbox.burn(&self.invite.session);
    }
}

/// The phone half: it holds the code, the device keypair (in memory until the
/// certificate arrives, so a failed pairing writes nothing) and the pinned
/// server key from the invite.
pub struct PairingPhone {
    invite: Invite,
    mailbox: MailboxClient,
    key: DeviceKey,
    mac_key: [u8; 32],
    deadline: Instant,
}

impl std::fmt::Debug for PairingPhone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingPhone")
            .field("session", &self.invite.session)
            .field("device", &DeviceId::from_key(&self.key.public()))
            .finish_non_exhaustive()
    }
}

impl PairingPhone {
    /// Join a pairing: read flight A, publish flight B and the hello. The
    /// device keypair is generated here and stays in memory until the
    /// certificate verifies.
    ///
    /// `key` is passed in when the caller already has one (re-pairing an
    /// existing device) and generated when not — the caller decides what is
    /// durable, this function decides what is correct.
    pub fn join(
        invite: &Invite,
        code: &Code,
        key: DeviceKey,
        name: &str,
    ) -> Result<Self, PairingError> {
        let mailbox = MailboxClient::new(invite.mailbox.clone());
        let deadline = Instant::now() + invite.ttl;
        let flight_a = mailbox.wait_for(&invite.session, Slot::A, deadline, POLL_INTERVAL)?;
        let (state, flight_b) = Spake2::<Ed25519Group>::start_b(
            &Password::new(code.password()),
            &identity_a(invite),
            &identity_b(invite),
        );
        mailbox.put(&invite.session, Slot::B, &flight_b)?;
        let shared = state
            .finish(&flight_a)
            .map_err(|e| PairingError::Spake(e.to_string()))?;
        let mac_key = derive_mac_key(&shared);

        let hello = PhoneHello {
            public_key: hex(&key.public().to_bytes()),
            name: name.to_string(),
        };
        let body = rmp_serde::to_vec_named(&hello)
            .map_err(|e| PairingError::Mailbox(format!("cannot encode the hello: {e}")))?;
        let framed = frame_mac(&mac_key, LABEL_PHONE, &invite.session, &body);
        mailbox.put(&invite.session, Slot::C, &framed)?;

        Ok(Self {
            invite: invite.clone(),
            mailbox,
            key,
            mac_key,
            deadline,
        })
    }

    /// Wait for the server's reply and verify the certificate against the
    /// **pinned** server key from the invite.
    pub fn await_cert(self) -> Result<PairedDevice, PairingError> {
        let reply =
            self.mailbox
                .wait_for(&self.invite.session, Slot::D, self.deadline, POLL_INTERVAL)?;
        let (body, tag) = split_mac(&reply, LABEL_SERVER, &self.invite.session)?;
        if !verify_mac(
            &self.mac_key,
            LABEL_SERVER,
            &self.invite.session,
            body,
            &tag,
        ) {
            return Err(PairingError::Confirmation);
        }
        let cert = DeviceCert::decode(body)
            .map_err(|e| PairingError::Mailbox(format!("malformed certificate: {e}")))?;
        // Two independent checks, both required:
        //  1. the certificate verifies under the key the *invite* pinned, and
        //  2. it is a certificate for the key this phone just generated.
        let server_key = self.invite.server_public_key()?;
        cert.verify(&server_key, &self.key.public())
            .map_err(|_| PairingError::Certificate)?;
        // Last reader retires the session: the certificate is in hand, so the
        // code that produced it must not open anything again. Best-effort — a
        // relay that has already forgotten the session (TTL) is not an error.
        let _ = self.mailbox.burn(&self.invite.session);
        Ok(PairedDevice {
            cert,
            key: self.key,
        })
    }
}

/// The phone's hello, signed under the shared key. MessagePack, like every
/// other structure in the project (ADR 0006).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct PhoneHello {
    /// Hex of the phone's ed25519 public key.
    public_key: String,
    name: String,
}

/// SPAKE2 identity for the server: its public key plus the protocol label, so
/// a phone cannot be tricked into pairing against a different machine (a
/// different identity yields a different key, which fails confirmation).
fn identity_a(invite: &Invite) -> Identity {
    Identity::new(format!("{ID_LABEL_SERVER}:{}", invite.server_key).as_bytes())
}

/// SPAKE2 identity for the phone side: the session id, so a flight from one
/// session cannot be replayed into another.
fn identity_b(invite: &Invite) -> Identity {
    Identity::new(format!("{ID_LABEL_SESSION}:{}", invite.session).as_bytes())
}

/// Derive the confirmation key from SPAKE2's output. The output is already a
/// high-entropy group element; the label keeps it from being reused if the
/// protocol grows a second purpose.
fn derive_mac_key(shared: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(MAC_KEY_LABEL);
    hasher.update(shared);
    hasher.finalize().into()
}

/// `HMAC(mac_key, label || len(label) || session || payload)`.
///
/// The session is inside the MAC: a flight cannot be lifted from one session
/// and replayed into another, even by the relay.
fn mac(mac_key: &[u8; 32], label: &[u8], session: &str, payload: &[u8]) -> [u8; 32] {
    let mut hmac = Hmac::<Sha256>::new_from_slice(mac_key).expect("HMAC accepts any key length");
    hmac.update(&(label.len() as u64).to_be_bytes());
    hmac.update(label);
    hmac.update(&(session.len() as u64).to_be_bytes());
    hmac.update(session.as_bytes());
    hmac.update(payload);
    hmac.finalize().into_bytes().into()
}

/// Constant-time verification (HMAC's own comparison).
fn verify_mac(mac_key: &[u8; 32], label: &[u8], session: &str, payload: &[u8], tag: &[u8]) -> bool {
    let mut hmac = Hmac::<Sha256>::new_from_slice(mac_key).expect("HMAC accepts any key length");
    hmac.update(&(label.len() as u64).to_be_bytes());
    hmac.update(label);
    hmac.update(&(session.len() as u64).to_be_bytes());
    hmac.update(session.as_bytes());
    hmac.update(payload);
    hmac.verify_slice(tag).is_ok()
}

/// `payload || tag`, tag last so the payload can be parsed before the check.
fn frame_mac(mac_key: &[u8; 32], label: &[u8], session: &str, payload: &[u8]) -> Vec<u8> {
    let mut out = payload.to_vec();
    out.extend_from_slice(&mac(mac_key, label, session, payload));
    out
}

/// Split `payload || tag`. A frame shorter than a tag is refused, and the
/// payload is never empty (an empty body would MAC fine and parse to nothing).
fn split_mac<'a>(
    framed: &'a [u8],
    label: &[u8],
    session: &str,
) -> Result<(&'a [u8], [u8; 32]), PairingError> {
    let _ = (label, session);
    if framed.len() <= 32 {
        return Err(PairingError::Confirmation);
    }
    let (body, tag) = framed.split_at(framed.len() - 32);
    let mut tag_bytes = [0u8; 32];
    tag_bytes.copy_from_slice(tag);
    Ok((body, tag_bytes))
}

/// A fresh session id: 128 bits of entropy, hex. Unguessable on purpose — the
/// mailbox is reachable by anyone who can reach the relay, so the id must not
/// be enumerable even though it is not the secret that authenticates.
fn random_session() -> Result<String, PairingError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| PairingError::Entropy(e.to_string()))?;
    Ok(hex(&bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn parse_public_key(text: &str) -> Result<VerifyingKey, PairingError> {
    let trimmed = text.trim();
    if trimmed.len() != 64 {
        return Err(PairingError::Identity(format!(
            "a public key is 64 hex characters, got {}",
            trimmed.len()
        )));
    }
    let mut bytes = [0u8; 32];
    for (index, slot) in bytes.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&trimmed[index * 2..index * 2 + 2], 16)
            .map_err(|_| PairingError::Identity("public key is not hex".to_string()))?;
    }
    VerifyingKey::from_bytes(&bytes)
        .map_err(|e| PairingError::Identity(format!("not an ed25519 point: {e}")))
}

/// Percent-encode everything outside the URI unreserved set. Written out rather
/// than pulled in as a dependency: it is fifteen lines and the invite is the
/// only thing that needs it.
fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

fn percent_decode(text: &str) -> Result<String, PairingError> {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(PairingError::BadInvite(format!(
                    "truncated percent escape in {text:?}"
                )));
            }
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3])
                .map_err(|_| PairingError::BadInvite(format!("bad escape in {text:?}")))?;
            let byte = u8::from_str_radix(hex, 16)
                .map_err(|_| PairingError::BadInvite(format!("bad escape %{hex} in {text:?}")))?;
            out.push(byte);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).map_err(|_| PairingError::BadInvite(format!("{text:?} is not UTF-8")))
}

/// Sign a payload with a device key — used by the tests (and by any future
/// out-of-band confirmation) so the signing path is exercised, not assumed.
#[must_use]
pub fn sign_with(key: &DeviceKey, payload: &[u8]) -> Signature {
    key.sign(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mailbox address this platform can actually express (a unix socket
    /// where that exists, TCP elsewhere).
    fn test_mailbox() -> MailboxAddr {
        #[cfg(unix)]
        {
            MailboxAddr::Unix("/tmp/arreo-relay.sock".into())
        }
        #[cfg(not(unix))]
        {
            MailboxAddr::Tcp("127.0.0.1:8770".to_string())
        }
    }

    fn invite() -> Invite {
        Invite {
            session: "0123456789abcdef0123456789abcdef".to_string(),
            mailbox: test_mailbox(),
            server_key: RootKey::from_seed([5u8; 32]).public_hex(),
            ttl: Duration::from_secs(300),
        }
    }

    #[test]
    fn the_invite_uri_round_trips_and_carries_no_code() {
        let invite = invite();
        let uri = invite.uri();
        assert!(uri.starts_with("arreo://pair?v=1&"), "{uri}");
        // The address is percent-encoded, so the URI survives a QR and a shell.
        #[cfg(unix)]
        assert!(uri.contains("mb=%2Ftmp%2Farreo-relay.sock"), "{uri}");
        #[cfg(not(unix))]
        assert!(uri.contains("mb=127.0.0.1%3A8770"), "{uri}");
        assert_eq!(Invite::parse_uri(&uri).expect("parses"), invite);
        // A code is typed by a human; it must never be in the URI.
        let code = Code::from_indices([1, 2, 3, 4]);
        for word in code.phrase().split(' ') {
            assert!(!uri.contains(word), "{word} leaked into the invite URI");
        }
    }

    #[test]
    fn a_partial_or_bad_invite_is_refused() {
        let full = invite().uri();
        for broken in [
            full.replace("&s=", "&x="),          // no session
            full.replace("&k=", "&x="),          // no server key
            full.replace("&mb=", "&x="),         // no mailbox
            full.replace("v=1", "v=9"),          // future version
            full.replace("ttl=300", "ttl=x"),    // non-numeric ttl
            "https://example.com".to_string(),   // not an arreo invite
            full.replace("mb=%2Ftmp", "mb=%ZZ"), // broken escape
        ] {
            assert!(
                matches!(Invite::parse_uri(&broken), Err(PairingError::BadInvite(_))),
                "{broken} was accepted"
            );
        }
    }

    #[test]
    fn two_phones_with_the_same_code_derive_the_same_key() {
        let code = Code::from_indices([9, 9, 9, 9]);
        let invite = invite();
        let (a_state, msg_a) = Spake2::<Ed25519Group>::start_a(
            &Password::new(code.password()),
            &identity_a(&invite),
            &identity_b(&invite),
        );
        let (b_state, msg_b) = Spake2::<Ed25519Group>::start_b(
            &Password::new(code.password()),
            &identity_a(&invite),
            &identity_b(&invite),
        );
        let key_a = derive_mac_key(&a_state.finish(&msg_b).expect("A finishes"));
        let key_b = derive_mac_key(&b_state.finish(&msg_a).expect("B finishes"));
        assert_eq!(key_a, key_b, "SPAKE2 must agree on the same code");
        // And the claims hold in both directions.
        let framed = frame_mac(&key_a, LABEL_PHONE, &invite.session, b"hello");
        let (body, tag) = split_mac(&framed, LABEL_PHONE, &invite.session).expect("split");
        assert!(verify_mac(&key_b, LABEL_PHONE, &invite.session, body, &tag));
    }

    #[test]
    fn a_wrong_code_derives_a_different_key_and_fails_confirmation() {
        let right = Code::from_indices([9, 9, 9, 9]);
        let wrong = Code::from_indices([9, 9, 9, 10]);
        assert_ne!(right.password(), wrong.password());
        let invite = invite();
        let (a_state, msg_a) = Spake2::<Ed25519Group>::start_a(
            &Password::new(right.password()),
            &identity_a(&invite),
            &identity_b(&invite),
        );
        let (b_state, msg_b) = Spake2::<Ed25519Group>::start_b(
            &Password::new(wrong.password()),
            &identity_a(&invite),
            &identity_b(&invite),
        );
        let key_a = derive_mac_key(&a_state.finish(&msg_b).expect("A finishes"));
        let key_b = derive_mac_key(&b_state.finish(&msg_a).expect("B finishes"));
        assert_ne!(key_a, key_b, "a wrong code must not produce the same key");
        let framed = frame_mac(&key_b, LABEL_PHONE, &invite.session, b"hello");
        let (body, tag) = split_mac(&framed, LABEL_PHONE, &invite.session).expect("split");
        assert!(
            !verify_mac(&key_a, LABEL_PHONE, &invite.session, body, &tag),
            "the server accepted a phone that guessed wrong"
        );
    }

    #[test]
    fn a_flight_cannot_be_moved_between_sessions_or_roles() {
        let key = [7u8; 32];
        let framed = frame_mac(&key, LABEL_PHONE, "session-one", b"body");
        let (body, tag) = split_mac(&framed, LABEL_PHONE, "session-one").expect("split");
        // Same key, different session: refused (the relay cannot relocate a flight).
        assert!(!verify_mac(&key, LABEL_PHONE, "session-two", body, &tag));
        // Same key and session, different role: refused (a phone's flight cannot
        // be replayed as the server's).
        assert!(!verify_mac(&key, LABEL_SERVER, "session-one", body, &tag));
        // Same key, tampered body: refused.
        let mut tampered = body.to_vec();
        tampered[0] ^= 1;
        assert!(!verify_mac(
            &key,
            LABEL_PHONE,
            "session-one",
            &tampered,
            &tag
        ));
    }

    #[test]
    fn a_truncated_or_empty_frame_is_refused_before_parsing() {
        assert!(matches!(
            split_mac(b"", LABEL_PHONE, "s"),
            Err(PairingError::Confirmation)
        ));
        assert!(matches!(
            split_mac(&[0u8; 32], LABEL_PHONE, "s"),
            Err(PairingError::Confirmation),
        ));
        let key = [1u8; 32];
        let framed = frame_mac(&key, LABEL_PHONE, "s", b"x");
        assert_eq!(framed.len(), 33);
    }

    #[test]
    fn the_identity_binding_separates_servers_and_sessions() {
        let one = invite();
        let mut other_server = invite();
        other_server.server_key = RootKey::from_seed([6u8; 32]).public_hex();
        let mut other_session = invite();
        other_session.session = "ffffffffffffffffffffffffffffffff".to_string();

        // Different server → different identity → different key: a MITM that
        // substitutes its own key cannot ride the same code. `Identity` derefs
        // to the bytes it was built from.
        let bytes = |identity: Identity| -> Vec<u8> { identity.to_vec() };
        assert_ne!(bytes(identity_a(&one)), bytes(identity_a(&other_server)));
        assert_ne!(bytes(identity_b(&one)), bytes(identity_b(&other_session)));
        assert_eq!(bytes(identity_b(&one)), bytes(identity_b(&one)));
    }

    #[test]
    fn session_ids_are_random_and_unguessable() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..64 {
            let id = random_session().expect("entropy");
            assert_eq!(id.len(), 32, "128 bits of hex");
            assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
            assert!(seen.insert(id), "session ids repeated");
        }
    }

    #[test]
    fn public_keys_parse_strictly() {
        let key = RootKey::from_seed([2u8; 32]);
        assert_eq!(
            parse_public_key(&key.public_hex())
                .expect("round trips")
                .to_bytes(),
            key.public().to_bytes()
        );
        for bad in ["", "abc", &"z".repeat(64), &"0".repeat(63)] {
            assert!(
                matches!(parse_public_key(bad), Err(PairingError::Identity(_))),
                "{bad:?} was accepted"
            );
        }
    }

    #[test]
    fn percent_encoding_round_trips_every_byte() {
        for text in [
            "/tmp/a b.sock",
            "host:8443",
            "a/b?c=d&e",
            "\u{00e9}\u{4e2d}",
        ] {
            assert_eq!(
                percent_decode(&percent_encode(text)).expect("round trips"),
                text
            );
        }
        assert_eq!(percent_encode("/tmp/x.sock"), "%2Ftmp%2Fx.sock");
        assert_eq!(percent_encode("host:8443"), "host%3A8443");
        assert!(matches!(
            percent_decode("%ZZ"),
            Err(PairingError::BadInvite(_))
        ));
        assert!(matches!(
            percent_decode("%2"),
            Err(PairingError::BadInvite(_))
        ));
    }
}
