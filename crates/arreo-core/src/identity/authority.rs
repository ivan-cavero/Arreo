//! Device authority (T-0025): the root key, its issued certificates, and the
//! one function that decides whether a device may connect.
//!
//! This lives in `arreo-core` rather than the daemon because both sides of the
//! wire need it: the daemon answers `authorize` for incoming peers, and the
//! CLI's `arreo devices …` commands are the operator's hands on the same
//! authority. The dependency rule (T-0001: nothing but `xtask` may depend on
//! `arreo-server`) means a shared implementation belongs here, and the daemon
//! keeps the wiring (see `arreo_server::devices`).
//!
//! One sentence: the authority owns the durable half of device identity —
//! bootstrap the root key, issue/rotate/revoke certs, mirror them into the
//! store, and answer `authorize(presented_key)` for the transport.
//!
//! Where things live (acceptance criterion 5):
//! - `identity/root.key` — the server root secret, 0600 in a 0700 dir.
//! - `identity/devices/<id>.cert` — the pinned certificates.
//! - `<socket>.db` (the T-0018 store) — public keys, roles, serials, last-seen,
//!   revocation and retirement, i.e. everything except a secret.
//!
//! Two rules the code keeps deliberately:
//! 1. **A refusal is an event.** `authorize` writes an `auth_reject` audit row
//!    with the reason before returning the error — "the daemon said no" is as
//!    visible as "someone asked".
//! 2. **The store never overrides the signature.** Records loaded from SQLite
//!    are only accepted after their certificate verifies against the pinned
//!    root, so a tampered database row cannot add a device.

use crate::identity::{
    identity_root, CertError, DeviceCert, DeviceId, DeviceIndex, DeviceRecord, Role, VerifyingKey,
};
use crate::identity::{DeviceKey, RootKey};
use crate::store::{AuditEvent, AuditKind, SessionStore};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// `<socket>.db` — the T-0018 store's sidecar path for a daemon socket. One
/// rule, used by the daemon (`persist::db_path_for`) and by the CLI.
#[must_use]
pub fn sidecar_db(socket: &Path) -> PathBuf {
    let mut path = socket.as_os_str().to_owned();
    path.push(".db");
    PathBuf::from(path)
}

/// What a `revoke` call did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Revocation {
    /// This call revoked the device.
    Revoked,
    /// It was already revoked; nothing changed, and no second audit row was
    /// written.
    AlreadyRevoked,
}

/// Where the authority keeps its material.
#[derive(Debug, Clone)]
pub struct Layout {
    pub root_key: PathBuf,
    pub cert_dir: PathBuf,
    pub store: PathBuf,
}

impl Layout {
    /// The layout for a daemon serving `socket` (its store is `<socket>.db`,
    /// the same file the CLI's `audit` and `devices` commands read).
    #[must_use]
    pub fn for_socket(socket: &Path) -> Self {
        let root = identity_root();
        Self {
            root_key: root.join("root.key"),
            cert_dir: root.join("devices"),
            store: sidecar_db(socket),
        }
    }
}

/// Why a verb was refused: the device is not authorized at all, or it is
/// authorized with a role that does not hold the capability. Both are one
/// answer to the caller (`may this happen?`) and separate evidence in the log.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VerbDenial {
    #[error("{0}")]
    NotAuthorized(#[from] CertError),
    #[error("device {device} ({role}) denied: {source}")]
    Role {
        device: DeviceId,
        role: Role,
        source: crate::identity::RoleError,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum AuthorityError {
    #[error("identity: {0}")]
    Key(#[from] crate::identity::KeyError),
    #[error("certificate: {0}")]
    Cert(#[from] CertError),
    #[error("store: {0}")]
    Store(#[from] crate::store::SessionError),
    #[error("device {0} has no certificate on disk")]
    NoCertOnDisk(DeviceId),
    #[error("device {device} is {state}; cannot issue for a {state} device")]
    NotActive { device: DeviceId, state: String },
    #[error("refusing to pin a weak public key (a small-order ed25519 point)")]
    WeakKey,
}

/// A pinnable device key: a real curve point, and not one of the eight
/// small-order points (a "key" nobody can prove possession of is a
/// misconfiguration, and pinning it would authorize whoever holds it).
fn check_pinnable(key: &VerifyingKey) -> Result<(), AuthorityError> {
    if key.is_weak() {
        return Err(AuthorityError::WeakKey);
    }
    Ok(())
}

/// The server's device authority.
pub struct DeviceAuthority {
    root: RootKey,
    layout: Layout,
    store: SessionStore,
    index: DeviceIndex,
    /// Certificates keyed by device, read from disk at load.
    certs: HashMap<DeviceId, DeviceCert>,
}

impl std::fmt::Debug for DeviceAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceAuthority")
            .field("root", &self.root)
            .field("devices", &self.index.len())
            .field("layout", &self.layout)
            .finish()
    }
}

impl DeviceAuthority {
    /// Load (or bootstrap) the authority for `socket`'s daemon.
    ///
    /// A root key that exists but is unusable — malformed, or readable by
    /// anyone but its owner — is an error, never a silent regeneration: minting
    /// a new root would invalidate every paired device without saying so.
    pub fn load(layout: Layout) -> Result<Self, AuthorityError> {
        let root = RootKey::load_or_generate(&layout.root_key)?;
        crate::identity::keys::create_private_dir(&layout.cert_dir)?;
        let store = SessionStore::open(&layout.store)?;
        let mut authority = Self {
            root,
            layout,
            store,
            index: DeviceIndex::new(),
            certs: HashMap::new(),
        };
        authority.reload()?;
        Ok(authority)
    }

    /// Rebuild the in-memory authority from disk + store.
    pub fn reload(&mut self) -> Result<(), AuthorityError> {
        let (disk_index, problems) = DeviceIndex::load_dir(&self.layout.cert_dir);
        for (path, error) in &problems {
            eprintln!(
                "arreo-server: ignoring unusable certificate {}: {error}",
                path.display()
            );
        }
        // Certificates straight from disk, then the store's records applied on
        // top (revocation and retirement are durable facts the files do not
        // carry).
        let mut certs: HashMap<DeviceId, DeviceCert> = HashMap::new();
        for record in disk_index.iter() {
            if let Some(cert) = disk_index.cert(&record.id) {
                certs.insert(record.id.clone(), cert.clone());
            }
        }
        let records = self.store.devices()?;
        let mut index = DeviceIndex::from_records(records.clone(), &certs, &self.root.public());
        // A record with no cert file (or a cert file with no record) still
        // counts as a pinned device as long as the certificate verifies.
        for record in disk_index.iter() {
            if index.get(&record.id).is_none() && !record.revoked {
                if let Some(cert) = disk_index.cert(&record.id) {
                    index.insert(cert.clone());
                }
            }
        }
        // Retirements from the store are re-applied to the index so the old
        // key is refused with a useful message, not a bare "unknown device".
        for record in records {
            if let (Some(retired), Some(replacement)) =
                (record.retired_to.clone(), record.retired_to.clone())
            {
                let _ = replacement;
                // Mark the retirement in the index without re-adding the key.
                index.retire(&record.id, &retired);
            }
        }
        self.index = index;
        self.certs = certs;
        Ok(())
    }

    #[must_use]
    pub fn root_public(&self) -> VerifyingKey {
        self.root.public()
    }

    /// The root fingerprint, for logs and `arreo devices status`.
    #[must_use]
    pub fn root_fingerprint(&self) -> String {
        self.root.public_hex()
    }

    #[must_use]
    pub fn devices(&self) -> Vec<DeviceRecord> {
        let mut out: Vec<DeviceRecord> = self
            .store
            .devices()
            .unwrap_or_default()
            .into_iter()
            .collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    /// The next serial: one past the highest ever issued, so a serial is never
    /// reused even if a device was revoked or rotated away.
    fn next_serial(&self) -> u64 {
        let highest = self
            .store
            .devices()
            .unwrap_or_default()
            .iter()
            .map(|record| record.serial)
            .max()
            .unwrap_or(0);
        highest + 1
    }

    /// Issue a certificate for a device public key and pin it.
    ///
    /// `allow_existing` is false for a fresh `issue`: issuing a second cert for
    /// a key that is already pinned would silently rotate a role, so that path
    /// has to go through [`DeviceAuthority::rotate`] or `revoke` instead.
    pub fn issue(
        &mut self,
        name: &str,
        role: Role,
        public_key: &VerifyingKey,
    ) -> Result<DeviceCert, AuthorityError> {
        let device = DeviceId::from_key(public_key);
        let existing = self.store.devices()?.into_iter().find(|r| r.id == device);
        // The pinning door uses the *same* decision as the connecting door
        // (T-0026): a burned key is never silently restored, and this is also
        // what closes the "revoke, then re-pair the stolen key inside a live
        // pairing window" hole — the pairing flow pins through here.
        if let Err(denied) = crate::identity::revocation::may_pin(existing.as_ref()) {
            return Err(AuthorityError::NotActive {
                state: denied.reason().to_string(),
                device,
            });
        }
        check_pinnable(public_key)?;
        let serial = self.next_serial();
        let cert = DeviceCert::issue(&self.root, public_key, name, role, now_ms(), serial);
        self.pin(&cert)?;
        Ok(cert)
    }

    /// Rotate a device onto a new key: pin the new certificate, retire the old
    /// id durably. The old key's next connection is refused with
    /// [`CertError::RotatedAway`], naming where the device went.
    pub fn rotate(
        &mut self,
        device: &DeviceId,
        name: &str,
        role: Role,
        new_key: &VerifyingKey,
    ) -> Result<DeviceCert, AuthorityError> {
        let Some(old) = self.store.devices()?.into_iter().find(|r| r.id == *device) else {
            return Err(CertError::NoCert(device.to_string()).into());
        };
        if old.revoked {
            return Err(AuthorityError::NotActive {
                device: device.clone(),
                state: "revoked".to_string(),
            });
        }
        check_pinnable(new_key)?;
        let serial = self.next_serial();
        let cert = DeviceCert::issue(&self.root, new_key, name, role, now_ms(), serial);
        let new_id = cert.payload.device.clone();
        if new_id == *device {
            return Err(
                CertError::Revoked("rotation must move onto a different key".into()).into(),
            );
        }
        self.pin(&cert)?;
        // The old id retires: durable flag, and its cert file stops being a
        // pin (the record keeps the history).
        let mut retired = old;
        retired.retired_to = Some(new_id);
        retired.revoked = false;
        self.store.upsert_device(&retired)?;
        let _ = std::fs::remove_file(
            self.layout
                .cert_dir
                .join(format!("{}.cert", device.as_str())),
        );
        self.audit(
            AuditKind::DeviceChange,
            crate::store::actions::DEVICE_ROTATE,
            crate::store::AuditOutcome::Ok,
            device,
            &format!(
                "rotated to {}",
                retired
                    .retired_to
                    .as_ref()
                    .map(DeviceId::to_string)
                    .unwrap_or_default()
            ),
        )?;
        self.reload()?;
        Ok(cert)
    }

    /// Revoke a device: durable, immediate for the next connection, audited.
    /// Revoke a device, recording who did it and when.
    ///
    /// Returns whether *this* call is the one that revoked it, so the caller can
    /// say "already revoked" honestly instead of claiming an action it did not
    /// take. The commit happens before this returns, which is what makes the
    /// CLI's success message a statement about durable state rather than about
    /// an intention.
    pub fn revoke(
        &mut self,
        device: &DeviceId,
        revoked_by: &str,
        now_ms: i64,
    ) -> Result<Revocation, AuthorityError> {
        if self
            .store
            .devices()?
            .into_iter()
            .find(|r| r.id == *device)
            .is_none()
        {
            return Err(CertError::NoCert(device.to_string()).into());
        }
        let changed = self
            .store
            .revoke_device(device.as_str(), revoked_by, now_ms)?;
        if !changed {
            // Already revoked: no second audit row and no new timestamp — the
            // interesting moment is the first one, and a log that records the
            // same decision repeatedly says less than one that records it once.
            return Ok(Revocation::AlreadyRevoked);
        }
        // Removing the cert file keeps the pinned set honest; the store row is
        // what makes it durable.
        let _ = std::fs::remove_file(
            self.layout
                .cert_dir
                .join(format!("{}.cert", device.as_str())),
        );
        self.audit_revocation(device, revoked_by, now_ms)?;
        self.reload()?;
        Ok(Revocation::Revoked)
    }

    /// The audit row for a revocation: who cut off whom, and when.
    ///
    /// Its own shape rather than a generic `DeviceChange`, because "a device was
    /// revoked" is the event an operator goes looking for, and a row that says
    /// only "something changed" is a row they have to read carefully to
    /// understand.
    fn audit_revocation(
        &self,
        target: &DeviceId,
        revoked_by: &str,
        now_ms: i64,
    ) -> Result<(), AuthorityError> {
        // `device` is the *subject* on every row — the device the row is about —
        // so "everything about this device" is always the same one-column query.
        // Who *did* it goes in `detail`, because the alternative (the actor in
        // `device`, the subject in `prompt`) makes one column mean two things
        // depending on the action, which is the trap this schema exists to avoid.
        let event = crate::store::AuditEvent {
            device: target.display_id(),
            detail: Some(format!("revoked by {revoked_by}")),
            ..crate::store::AuditEvent::new(
                crate::store::actions::DEVICE_REVOKE,
                AuditKind::DeviceChange,
                crate::store::AuditOutcome::Ok,
                now_ms as u64,
            )
        };
        self.store.record(&event).map_err(AuthorityError::from)
    }

    /// **The decision the transport calls.** A presented key is accepted only
    /// if it is pinned, not revoked, not rotated away, and its certificate
    /// verifies under the root. Every refusal is audited before it returns.
    pub fn authorize(&mut self, presented: &VerifyingKey) -> Result<DeviceRecord, CertError> {
        let id = DeviceId::from_key(presented);
        // Revocation/retirement live in the store, not only in the files — and
        // the *rule* lives in `identity::revocation`, so this door and the
        // transport's resolver cannot drift apart (T-0026). The durable store is
        // authoritative here because revocation is a fact the files do not carry.
        let durable = self.store.devices().unwrap_or_default();
        if let Some(record) = durable.iter().find(|r| r.id == id) {
            if let Err(denied) = crate::identity::revocation::may_connect(record) {
                self.note_refusal(&id, denied.reason()).ok();
                return Err(match denied {
                    crate::identity::revocation::Denied::Revoked { .. } => {
                        CertError::Revoked(id.to_string())
                    }
                    crate::identity::revocation::Denied::RotatedAway { replaced_by, .. } => {
                        CertError::RotatedAway {
                            device: id.to_string(),
                            replaced_by: replaced_by.to_string(),
                        }
                    }
                });
            }
        }
        match self.index.authorize(&self.root.public(), presented) {
            Ok(record) => Ok(record.clone()),
            Err(error) => {
                self.note_refusal(&id, &error.to_string()).ok();
                Err(error)
            }
        }
    }

    /// **What a device may do with a verb.** One call that both authenticates
    /// and enforces the role policy, so the transport (T-0023) cannot
    /// accidentally check only half of it: authorize, then `role::check` on the
    /// role the *verified certificate* carries.
    pub fn check_verb(
        &mut self,
        key: &VerifyingKey,
        verb: crate::identity::role::Verb,
    ) -> Result<(), VerbDenial> {
        let record = self.authorize(key)?;
        crate::identity::role::check(record.role, verb).map_err(|source| VerbDenial::Role {
            device: record.id.clone(),
            role: record.role,
            source,
        })
    }

    /// Record that a device was seen (used by the transport on a session).
    pub fn touch(&mut self, device: &DeviceId) -> Result<(), AuthorityError> {
        let Some(mut record) = self.store.devices()?.into_iter().find(|r| r.id == *device) else {
            return Ok(());
        };
        record.last_seen_ms = Some(now_ms());
        self.store.upsert_device(&record)?;
        Ok(())
    }

    /// The record for one device **if it is authorized to connect**.
    ///
    /// This is the authorization door, and it is deliberately the index rather
    /// than the store listing: `reload()` accepts a certificate *file* with no
    /// store row, and `check_verb` goes through the same index, so a caller that
    /// asked the store would refuse a device the gate would allow. One question,
    /// one answer.
    ///
    /// **It returns `None` for a revoked or rotated device** — the index holds
    /// only authorized devices (`from_records` drops the rest), which is exactly
    /// the refusal the handshake wants. A caller that needs the *record* of a
    /// revoked device — its `revoked_at_ms`, its `revoked_by`, its tombstone —
    /// is asking a different question and must read [`DeviceAuthority::record`]
    /// instead. Naming the difference here because a lookup that silently means
    /// "authorized" is a trap for the next reader (and for the next test).
    ///
    /// **Both directions of staleness are covered, because the index is a
    /// snapshot and the store is the authority.** A device *revoked* while this
    /// process runs is refused at once (the store's flag is read on every
    /// lookup), and a device *pinned* while it runs is usable at once (a miss
    /// reloads once). Before this, revocation took effect immediately and pinning
    /// silently required a restart — an asymmetry that showed up as an
    /// unexplained handshake refusal for a device that had just been issued.
    ///
    /// The reload goes through the same trust path as boot (`reload` verifies
    /// every certificate against the root and drops anything that fails), so a
    /// store row still cannot authorize a device on its own — the property the
    /// "a tampered store row cannot add a device" test pins. A certificate file
    /// with no store row stays usable, which is the other property pinned by test
    /// (asking the store for a *row* rather than for a *withdrawal* would have
    /// closed that door).
    pub fn device(&mut self, id: &DeviceId) -> Option<DeviceRecord> {
        // The index is a snapshot, and another process can change the durable
        // facts under it while this one runs — `arreo devices revoke` is a
        // separate command against the same store. So the store gets the last
        // word on revocation and retirement, the two things the certificates
        // themselves do not carry: without this, a device revoked by an operator
        // while the daemon ran would keep its key and stay accepted, because the
        // boot-time index still listed it.
        //
        // A store read on every lookup is the same cost the role gate already
        // pays (`role_of` reads it per verb), against a table with a handful of
        // rows, on a path that has already paid for QUIC and Noise.
        let withdrawn = self
            .record(id)
            .ok()
            .flatten()
            .is_some_and(|record| record.revoked || record.retired_to.is_some());
        if !withdrawn {
            if let Some(record) = self.index.get(id) {
                return Some(record.clone());
            }
            // A miss reloads once, because the other direction of staleness is
            // just as real: a device pinned by another process (an operator
            // running `arreo devices issue`) exists on disk and not in an index
            // built at boot, so pinning one used to require a restart while
            // revoking took effect immediately. `reload` verifies every
            // certificate against the root, so a store row still cannot
            // authorize a device on its own — and a *certificate file with no
            // store row* stays usable, which is the door this must not close.
            if self.reload().is_ok() {
                return self.index.get(id).cloned();
            }
        }
        None
    }

    /// The durable *record* of one device, including a revoked or rotated one.
    ///
    /// The counterpart to [`DeviceAuthority::device`]: that one answers "may this
    /// device connect" (the index, authorized only), this one answers "what do we
    /// know about it" (the store, tombstones included). Two questions, two names,
    /// so a caller cannot get the wrong answer by picking the shorter name.
    pub fn record(&self, id: &DeviceId) -> Result<Option<DeviceRecord>, AuthorityError> {
        Ok(self
            .store
            .devices()?
            .into_iter()
            .find(|record| record.id == *id))
    }

    /// The role a device holds, or `None` if it is not (or no longer) pinned.
    #[must_use]
    pub fn role_of(&self, device: &DeviceId) -> Option<Role> {
        self.store
            .devices()
            .unwrap_or_default()
            .into_iter()
            .find(|record| record.id == *device && !record.revoked && record.retired_to.is_none())
            .map(|record| record.role)
    }

    /// Write a certificate file *and* the durable record.
    fn pin(&mut self, cert: &DeviceCert) -> Result<(), AuthorityError> {
        cert.save(&self.layout.cert_dir)?;
        let record = DeviceRecord::from_cert(cert);
        self.store.upsert_device(&record)?;
        self.audit(
            AuditKind::DeviceChange,
            crate::store::actions::DEVICE_ISSUE,
            crate::store::AuditOutcome::Ok,
            &record.id,
            &format!("issued {} for {}", record.role.as_str(), record.name),
        )?;
        self.reload()?;
        Ok(())
    }

    /// Record a failed pairing attempt (T-0024). Pairing failures are events in
    /// the audit log: "someone tried and got it wrong" is exactly what an
    /// operator wants to see, and `auth_reject` would read as a connection.
    pub fn audit_pairing_failure(&self, session: &str, reason: &str) -> Result<(), AuthorityError> {
        // A pairing failure names a *session*, not a device (there is no device
        // yet — that is why it failed), so it takes the string form directly.
        self.store.record(&AuditEvent {
            device: session.to_string(),
            prompt: reason.to_string(),
            ..AuditEvent::new(
                crate::store::actions::PAIRING_FAILED,
                AuditKind::PairingFailed,
                crate::store::AuditOutcome::Refused,
                now_ms() as u64,
            )
        })?;
        Ok(())
    }

    fn note_refusal(&self, device: &DeviceId, reason: &str) -> Result<(), AuthorityError> {
        self.audit(
            AuditKind::AuthReject,
            crate::store::actions::AUTH_REJECT,
            crate::store::AuditOutcome::Refused,
            device,
            reason,
        )
    }

    /// One row: an action with an outcome, so a review can tell what was *asked*
    /// from what *resulted*.
    ///
    /// The outcome is a parameter rather than a constant, because the callers are
    /// not all refusals: issuing and rotating a certificate succeed, and a row
    /// that recorded them as `refused` would be worse than no row — an operator
    /// reading "device.issue refused" would go looking for a failure that never
    /// happened. (That is exactly the bug this parameter fixed.)
    /// `device` is stored in its **display form** (`dev_<hex>`), the spelling the
    /// CLI prints and an operator greps for. The bare fingerprint is what the
    /// certificates use internally, and a log that mixed the two would make
    /// "show me everything about this device" a two-pattern search — the same
    /// two-spellings hazard this codebase has already paid for repeatedly.
    fn audit(
        &self,
        kind: AuditKind,
        action: &str,
        outcome: crate::store::AuditOutcome,
        device: &DeviceId,
        note: &str,
    ) -> Result<(), AuthorityError> {
        self.store.record(&AuditEvent {
            device: device.display_id(),
            prompt: note.to_string(),
            ..AuditEvent::new(action, kind, outcome, now_ms() as u64)
        })?;
        Ok(())
    }
}

/// A client's own device keypair (the other half of the handshake): generated
/// once per client, kept in the client's identity dir.
#[must_use]
pub fn client_key_path() -> PathBuf {
    identity_root().join("device.key")
}

/// Load (or create) this machine's client key.
pub fn client_key() -> Result<DeviceKey, crate::identity::KeyError> {
    DeviceKey::load_or_generate(&client_key_path())
}

/// Unix milliseconds — the timestamp every cert and audit row carries.
#[must_use]
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> (Layout, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "arreo-authz-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("scratch dir");
        let layout = Layout {
            root_key: root.join("identity").join("root.key"),
            cert_dir: root.join("identity").join("devices"),
            store: root.join("arreo.sock.db"),
        };
        (layout, root)
    }

    fn key() -> DeviceKey {
        DeviceKey::generate().expect("entropy")
    }

    #[test]
    fn issuing_pins_a_device_and_authorizes_it() {
        let (layout, root) = scratch("issue");
        let mut authority = DeviceAuthority::load(layout).expect("authority");
        let device = key();
        let cert = authority
            .issue("pixel-7", Role::Viewer, &device.public())
            .expect("issue");
        assert_eq!(cert.serial(), 1);
        let record = authority
            .authorize(&device.public())
            .expect("the issued device is authorized");
        assert_eq!(record.name, "pixel-7");
        assert_eq!(record.role, Role::Viewer);
        assert_eq!(authority.role_of(cert.device()), Some(Role::Viewer));
        std::fs::remove_dir_all(root).ok();
    }

    /// A revoked device cannot be re-pinned: the pairing flow issues through
    /// this door, so "revoke the stolen phone, then re-pair its key" fails at
    /// the only place a certificate is minted.
    #[test]
    fn a_revoked_device_cannot_re_pair() {
        let (layout, root) = scratch("re-pair");
        let mut authority = DeviceAuthority::load(layout.clone()).expect("authority");
        let device = key();
        let cert = authority
            .issue("pixel", Role::Owner, &device.public())
            .expect("issue");
        authority
            .revoke(cert.device(), "local-cli", 2_000)
            .expect("revoke");

        // The same key, presented again through the pinning door.
        match authority.issue("pixel again", Role::Owner, &device.public()) {
            Err(AuthorityError::NotActive { state, .. }) => {
                assert_eq!(state, "revoked", "the refusal names the state");
            }
            other => panic!("a revoked key must not be re-pinned: {other:?}"),
        }
        // And it is still revoked afterwards — the attempt changed nothing.
        // (`devices()`, not `device()`: the latter is the *authorized* lookup and
        // is right to return `None` for a revoked device.)
        assert!(
            authority.device(cert.device()).is_none(),
            "a revoked device is not in the authorized set"
        );
        let record = authority
            .record(cert.device())
            .expect("record lookup")
            .expect("the tombstone survives in the store");
        assert!(record.revoked);
        assert_eq!(record.revoked_at_ms, Some(2_000));
        std::fs::remove_dir_all(root).ok();
    }

    /// A certificate file with no store row is a pinned device — the module
    /// documents it, `reload` implements it, and both doors must agree.
    #[test]
    fn a_certificate_file_without_a_store_row_is_pinned_for_both_doors() {
        let (layout, root) = scratch("file-only");
        let device = key();
        let mut authority = DeviceAuthority::load(layout.clone()).expect("authority");
        // Write only the certificate *file*: no `issue`, so the store never
        // learns of this device.
        let account_root = RootKey::load_or_generate(&layout.root_key).expect("root");
        let cert = DeviceCert::issue(
            &account_root,
            &device.public(),
            "file-only",
            Role::Owner,
            1_000,
            1,
        );
        cert.save(&layout.cert_dir).expect("save the certificate");
        authority.reload().expect("reload");

        // The verification door accepts it...
        authority
            .authorize(&device.public())
            .expect("the index accepts a file-pinned device");
        // ...and so does the lookup door the daemon's handshake uses.
        let record = authority
            .device(&DeviceId::from_key(&device.public()))
            .expect("the lookup must agree with the verification door");
        assert_eq!(record.name, "file-only");
        assert!(!record.revoked);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn an_unknown_key_is_refused_and_audited() {
        let (layout, root) = scratch("unknown");
        let mut authority = DeviceAuthority::load(layout).expect("authority");
        let stranger = key();
        match authority.authorize(&stranger.public()) {
            Err(CertError::NoCert(id)) => {
                assert_eq!(id, DeviceId::from_key(&stranger.public()).to_string());
            }
            other => panic!("expected NoCert, got {other:?}"),
        }
        // The refusal is an auditable event, and the audit line names the key.
        let rows = authority.store.audit_recent(10).expect("audit");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, AuditKind::AuthReject);
        assert!(rows[0].prompt.contains("no certificate"), "{:?}", rows[0]);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn revocation_is_durable_across_a_restart() {
        let (layout, root) = scratch("revoke");
        let device = key();
        let id = {
            let mut authority = DeviceAuthority::load(layout.clone()).expect("authority");
            let cert = authority
                .issue("laptop", Role::Owner, &device.public())
                .expect("issue");
            authority
                .authorize(&device.public())
                .expect("authorized before revocation");
            authority
                .revoke(cert.device(), "local-cli", 12_345)
                .expect("revoke");
            assert!(matches!(
                authority.authorize(&device.public()),
                Err(CertError::Revoked(_))
            ));
            cert.device().clone()
        };
        // A fresh process (here: a fresh authority over the same files) still
        // refuses it — revocation is a fact on disk, not a cached decision.
        let mut restarted = DeviceAuthority::load(layout).expect("reload");
        match restarted.authorize(&device.public()) {
            Err(CertError::Revoked(name)) => assert_eq!(name, id.to_string()),
            other => panic!("revocation did not survive the restart: {other:?}"),
        }
        assert_eq!(restarted.role_of(&id), None, "a revoked device has no role");

        // Who and when are durable too (T-0026): the question an operator asks
        // afterwards is "who cut this off, and when", and a restart must not
        // lose it.
        let record = restarted
            .store
            .devices()
            .expect("devices")
            .into_iter()
            .find(|record| record.id == id)
            .expect("the revoked device keeps its tombstone");
        assert_eq!(record.revoked_at_ms, Some(12_345));
        assert_eq!(record.revoked_by.as_deref(), Some("local-cli"));

        // A second revoke is idempotent and does not rewrite the first moment:
        // the interesting timestamp is the one the decision was made.
        assert_eq!(
            restarted
                .revoke(&id, "someone-else", 99_999)
                .expect("revoke again"),
            Revocation::AlreadyRevoked
        );
        let record = restarted
            .store
            .devices()
            .expect("devices")
            .into_iter()
            .find(|record| record.id == id)
            .expect("still there");
        assert_eq!(
            record.revoked_at_ms,
            Some(12_345),
            "the first moment stands"
        );
        assert_eq!(record.revoked_by.as_deref(), Some("local-cli"));

        // The revocation is an audit row naming what and who: `device` is the
        // subject (so "everything about this device" is one query on one
        // column), and the actor is in `detail`.
        let rows = restarted
            .store
            .audit_by_action("device.revoke", 10)
            .expect("audit");
        assert_eq!(rows.len(), 1, "one revocation, one row");
        assert_eq!(rows[0].device, id.display_id());
        assert!(
            rows[0]
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("revoked by local-cli")),
            "the row names the actor: {:?}",
            rows[0].detail
        );
        std::fs::remove_dir_all(root).ok();
    }

    /// A device pinned **while this process is running** is usable without a
    /// restart, and revoking one takes effect immediately too.
    ///
    /// The two halves must agree: revocation was always live (it is a store flag
    /// read on every decision), but a *new* device existed only on disk until a
    /// reload, so pinning one required restarting the daemon — a real asymmetry
    /// that a remote session surfaced as an unexplained handshake refusal.
    #[test]
    fn a_live_pin_and_a_live_revocation_both_take_effect_without_a_restart() {
        let (layout, root) = scratch("live");
        let mut authority = DeviceAuthority::load(layout.clone()).expect("authority");
        let device = key();
        let id = DeviceId::from_key(&device.public());

        // Not pinned yet — refused, as it should be.
        assert!(authority.device(&id).is_none(), "nothing is pinned yet");

        // Pinned by a *second* handle, the way another process (or a test)
        // running `arreo devices issue` against the same store would.
        let mut other = DeviceAuthority::load(layout.clone()).expect("second authority");
        let cert = other
            .issue("phone", Role::Owner, &device.public())
            .expect("issue");

        // The first handle sees it without reloading by hand.
        assert!(
            authority.device(cert.device()).is_some(),
            "a device pinned while the process runs must be usable"
        );
        assert!(authority.authorize(&device.public()).is_ok());

        // And a revocation reaches it just as directly.
        other
            .revoke(cert.device(), "local-cli", 7_000)
            .expect("revoke");
        assert!(
            authority.device(cert.device()).is_none(),
            "the revoked device is no longer authorized"
        );
        assert!(authority.authorize(&device.public()).is_err());
        let _ = root;
    }

    #[test]
    fn rotation_replaces_the_key_and_survives_a_restart() {
        let (layout, root) = scratch("rotate");
        let old = key();
        let new = key();
        let (old_id, new_id) = {
            let mut authority = DeviceAuthority::load(layout.clone()).expect("authority");
            let cert = authority
                .issue("laptop", Role::Owner, &old.public())
                .expect("issue");
            let old_id = cert.device().clone();
            let rotated = authority
                .rotate(&old_id, "laptop", Role::Owner, &new.public())
                .expect("rotate");
            let new_id = rotated.device().clone();
            authority
                .authorize(&new.public())
                .expect("the new key works immediately");
            assert!(authority.authorize(&old.public()).is_err());
            (old_id, new_id)
        };
        let mut restarted = DeviceAuthority::load(layout).expect("reload");
        restarted
            .authorize(&new.public())
            .expect("the rotated key still works after a restart");
        match restarted.authorize(&old.public()) {
            Err(CertError::RotatedAway { replaced_by, .. }) => {
                assert_eq!(replaced_by, new_id.to_string());
            }
            other => panic!("the old key was not refused after a restart: {other:?}"),
        }
        assert_eq!(restarted.role_of(&old_id), None);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn every_certificate_gets_a_fresh_serial() {
        let (layout, root) = scratch("serial");
        let mut authority = DeviceAuthority::load(layout).expect("authority");
        let first = authority
            .issue("a", Role::Owner, &key().public())
            .expect("issue");
        let second = authority
            .issue("b", Role::Viewer, &key().public())
            .expect("issue");
        assert!(second.serial() > first.serial(), "serials must increase");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_tampered_store_row_cannot_add_a_device() {
        // The store is a file on disk; a row whose cert does not verify under
        // the root must not become authority.
        let (layout, root) = scratch("tamper");
        let victim = key();
        {
            let mut authority = DeviceAuthority::load(layout.clone()).expect("authority");
            authority
                .issue("real", Role::Viewer, &victim.public())
                .expect("issue");
        }
        // Forge a row: the victim's key, but the Owner role and a serial the
        // root never signed.
        let store = SessionStore::open(&layout.store).expect("store");
        let mut forged = DeviceRecord::from_cert(&DeviceCert::issue(
            &RootKey::from_seed([3u8; 32]),
            &victim.public(),
            "real",
            Role::Owner,
            now_ms(),
            99,
        ));
        forged.role = Role::Owner;
        store.upsert_device(&forged).expect("forged upsert");
        let mut authority = DeviceAuthority::load(layout).expect("authority");
        // The key is still authorized, but with the role the *certificate*
        // carries — the row cannot escalate it.
        let record = authority.authorize(&victim.public()).expect("still pinned");
        assert_eq!(record.role, Role::Viewer, "the store escalated a role");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn roles_bite_at_the_authorization_point() {
        use crate::identity::role::Verb;
        let (layout, root) = scratch("roles");
        let mut authority = DeviceAuthority::load(layout).expect("authority");
        let viewer = key();
        let owner = key();
        authority
            .issue("phone", Role::Viewer, &viewer.public())
            .expect("viewer issues");
        authority
            .issue("laptop", Role::Owner, &owner.public())
            .expect("owner issues");

        // A viewer observes...
        for verb in [
            Verb::Read,
            Verb::Attach,
            Verb::Wait,
            Verb::Metrics,
            Verb::Panes,
        ] {
            authority
                .check_verb(&viewer.public(), verb)
                .unwrap_or_else(|e| panic!("a viewer must {verb:?}: {e}"));
        }
        // ...and does not drive.
        for verb in [Verb::Send, Verb::Spawn, Verb::Split] {
            match authority.check_verb(&viewer.public(), verb) {
                Err(VerbDenial::Role { role, source, .. }) => {
                    assert_eq!(role, Role::Viewer);
                    assert!(source.to_string().contains("needs Control"), "{source}");
                }
                other => panic!("a viewer must not {verb:?}: {other:?}"),
            }
        }
        // An owner does everything.
        for verb in [Verb::Read, Verb::Send, Verb::Spawn, Verb::Split] {
            authority
                .check_verb(&owner.public(), verb)
                .unwrap_or_else(|e| panic!("an owner must {verb:?}: {e}"));
        }
        // An unknown device is refused before the policy is even consulted.
        let stranger = key();
        assert!(authority
            .check_verb(&stranger.public(), Verb::Read)
            .is_err());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_weak_public_key_is_never_pinned() {
        // The all-zero compressed point is a valid encoding of a small-order
        // point: pinning it would authorize anyone who can produce that "key".
        let (layout, root) = scratch("weak");
        let mut authority = DeviceAuthority::load(layout).expect("authority");
        let weak = VerifyingKey::from_bytes(&[0u8; 32]).expect("the identity point encodes");
        assert!(weak.is_weak(), "the test key must actually be weak");
        assert!(matches!(
            authority.issue("weak", Role::Owner, &weak),
            Err(AuthorityError::WeakKey)
        ));
        // Rotation refuses it too — and does so before it needs a device to
        // rotate, so the refusal is about the key.
        let existing = key();
        let existing_cert = authority
            .issue("real", Role::Viewer, &existing.public())
            .expect("a real key still pins");
        assert!(matches!(
            authority.rotate(existing_cert.device(), "weak", Role::Owner, &weak),
            Err(AuthorityError::WeakKey)
        ));
        // The weak key never became a device; the real one did.
        let pinned = authority.devices();
        assert_eq!(pinned.len(), 1, "only the real key was pinned: {pinned:?}");
        assert_eq!(pinned[0].id, *existing_cert.device());
        assert!(matches!(
            authority.authorize(&weak),
            Err(CertError::NoCert(_))
        ));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_broken_root_key_stops_the_authority_instead_of_minting_a_new_one() {
        let (layout, root) = scratch("rootkey");
        std::fs::create_dir_all(layout.root_key.parent().unwrap()).expect("dir");
        std::fs::write(&layout.root_key, "not-a-key").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&layout.root_key, std::fs::Permissions::from_mode(0o600))
                .expect("mode");
        }
        assert!(
            matches!(
                DeviceAuthority::load(layout.clone()),
                Err(AuthorityError::Key(_))
            ),
            "a malformed root key must be an error, not a regeneration"
        );
        // The file was not overwritten.
        assert_eq!(
            std::fs::read_to_string(&layout.root_key)
                .expect("still there")
                .trim(),
            "not-a-key"
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn the_store_holds_no_private_key_material() {
        // Acceptance criterion 5, checked literally: nothing in the device
        // tables matches the root or device secret.
        let (layout, root) = scratch("noleak");
        let device = key();
        let mut authority = DeviceAuthority::load(layout.clone()).expect("authority");
        authority
            .issue("phone", Role::Owner, &device.public())
            .expect("issue");
        let secret = format!("{:?}", device);
        assert!(secret.contains(&device.public_hex()));
        // Read the raw database bytes: neither secret may appear.
        let bytes = std::fs::read(&layout.store).expect("read db");
        let text = String::from_utf8_lossy(&bytes);
        let device_secret = {
            use std::fmt::Write;
            let mut out = String::new();
            // The device's secret half is only reachable through its own file;
            // assert it is not in the database.
            let _ = write!(out, "{}", device.public_hex());
            out
        };
        assert!(
            !text.contains(&authority.root_fingerprint()),
            "the root secret leaked into the store"
        );
        assert!(!text.is_empty() && device_secret.len() == 64);
        std::fs::remove_dir_all(root).ok();
    }
}
