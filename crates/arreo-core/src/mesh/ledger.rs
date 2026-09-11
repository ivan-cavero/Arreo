//! A machine's trust ledger: who may use *this* machine (T-0046, ROADMAP §3.7).
//!
//! One sentence: the machine decides who may touch it, from its own store,
//! against its own identity — the account's certificate says who a device *is*,
//! and this says what it may do *here*.
//!
//! [`crate::mesh::trust`] holds the rule (the record, the capabilities, the
//! refusals). This holds the machine-local half: which machine these rows belong
//! to, how a grant is made and cut, and the one-time backfill that keeps machines
//! paired before this existed from being locked out of their own devices.
//!
//! ## Why this lives in `arreo-core` and not in the daemon
//!
//! **A grant is written where a certificate is created**, and that is not always
//! the daemon: `arreo devices issue` and the server half of `arreo pair` run in
//! the *CLI* process against the authority directly (pairing may run with no
//! daemon at all). The dependency rule forbids `arreo-cli` depending on
//! `arreo-server`, so a ledger that lived in the daemon could not be written by
//! the command that creates devices — which would leave every newly issued device
//! ungranted and refused by the gate this ledger feeds. The store layer
//! (`SessionStore::record_trust`) was already here; this is its policy skin.
//!
//! ## The two gates, in order
//!
//! 1. **Who are you?** The certificate: pinned, not revoked, verifies under the
//!    account root (`DeviceAuthority::check_verb`). This is authentication, and
//!    it is the same on every machine of the account.
//! 2. **May you, here?** This ledger: a live grant for `(this machine, device)`
//!    whose role holds the verb's capability.
//!
//! Both must pass, which is what makes "a phone paired to the VPS is not
//! automatically paired to the Pi" true rather than aspirational — the phone's
//! certificate is perfectly valid on the Pi, and the Pi has no grant row for it.

use crate::identity::role::Verb;
use crate::identity::{DeviceId, Role};
use crate::mesh::{denial_message, evaluate, MachineId, TrustRecord};
use crate::store::SessionStore;
use std::path::Path;
use std::sync::Mutex;

/// A device's access to one machine, as a value an operator can read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantedDevice {
    pub device: DeviceId,
    pub role: Role,
    pub granted_at_ms: i64,
    pub granted_by: DeviceId,
    /// `None` while live.
    pub revoked_at_ms: Option<i64>,
}

impl GrantedDevice {
    #[must_use]
    pub fn is_live(&self) -> bool {
        self.revoked_at_ms.is_none()
    }
}

/// The machine's own ledger: its identity, its store, and the rule.
pub struct TrustLedger {
    store: SessionStore,
    machine: MachineId,
    /// The machine's human name, for refusals. A refusal that says
    /// `MachineId` is one an operator cannot act on, and the command it
    /// recommends needs the name.
    machine_name: String,
}

impl std::fmt::Debug for TrustLedger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrustLedger")
            .field("machine", &self.machine)
            .field("name", &self.machine_name)
            .finish_non_exhaustive()
    }
}

impl TrustLedger {
    /// Open the ledger for the machine that owns this store.
    ///
    /// `machine` is the machine's own root key identity (T-0056's rule: one
    /// definition of "which machine is this", shared with the directory row), and
    /// `machine_name` is what a human calls it.
    #[must_use]
    pub fn new(store: SessionStore, machine: MachineId, machine_name: String) -> Self {
        Self {
            store,
            machine,
            machine_name,
        }
    }

    /// Open the ledger against a store file, deriving the machine identity from
    /// the root key at `root_key`.
    ///
    /// The key is loaded (or generated) here because the machine's identity has
    /// exactly one source, and a ledger keyed by anything else would be a second
    /// answer to "which machine granted this".
    pub fn open(
        db: &Path,
        root_key: &Path,
        machine_name: String,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let root = crate::identity::RootKey::load_or_generate(root_key)?;
        let store = SessionStore::open(db)?;
        Ok(Self::new(
            store,
            MachineId::from_key(&root.public()),
            machine_name,
        ))
    }

    #[must_use]
    pub fn machine(&self) -> &MachineId {
        &self.machine
    }

    #[must_use]
    pub fn machine_name(&self) -> &str {
        &self.machine_name
    }

    /// Every grant this machine holds, live and revoked, sorted by device.
    pub fn devices(&self) -> Result<Vec<GrantedDevice>, crate::store::SessionError> {
        let mut rows: Vec<GrantedDevice> = self
            .store
            .trust_records()?
            .into_iter()
            .filter(|record| record.machine_id == self.machine)
            .map(|record| GrantedDevice {
                device: record.device_id,
                role: record.role,
                granted_at_ms: record.granted_at_ms,
                granted_by: record.granted_by,
                revoked_at_ms: record.revoked_at_ms,
            })
            .collect();
        rows.sort_by(|a, b| a.device.as_str().cmp(b.device.as_str()));
        Ok(rows)
    }

    /// This machine's grant to one device, if there is a row at all.
    fn record(&self, device: &DeviceId) -> Result<Option<TrustRecord>, crate::store::SessionError> {
        Ok(self
            .store
            .trust_records()?
            .into_iter()
            .find(|record| record.machine_id == self.machine && &record.device_id == device))
    }

    /// Grant (or re-grant) `device` a role on this machine.
    ///
    /// `by` is the device making the grant, so the row records whether a device
    /// was admitted to itself (the pairing default) or extended by another device
    /// — the distinction an audit trail exists to preserve.
    pub fn grant(
        &self,
        device: &DeviceId,
        role: Role,
        by: &DeviceId,
        now_ms: i64,
    ) -> Result<TrustRecord, crate::store::SessionError> {
        let record = TrustRecord {
            machine_id: self.machine.clone(),
            device_id: device.clone(),
            role,
            granted_at_ms: now_ms,
            granted_by: by.clone(),
            revoked_at_ms: None,
        };
        self.store.record_trust(&record)?;
        Ok(record)
    }

    /// Cut this machine's grant to `device`. `Ok(false)` means there was nothing
    /// live to cut, which the caller reports as such rather than as success.
    pub fn revoke(
        &self,
        device: &DeviceId,
        now_ms: i64,
    ) -> Result<bool, crate::store::SessionError> {
        self.store.revoke_trust(&self.machine, device, now_ms)
    }

    /// The rule, applied to this machine's ledger, with the refusal an operator
    /// can act on.
    ///
    /// The refusal is built here and nowhere else: T-0046 requires a denial to
    /// name the machine, the missing role and the exact granting command, and one
    /// builder is what stops a refusal path from forgetting one of the three.
    ///
    /// A store read that fails is **not** a refusal — it is an error, and the
    /// caller must not read it as "no grant". Fail-closed on the store would end
    /// every session on a transient SQLite error; the honest split is that a
    /// *missing row* refuses and an *unreadable store* is a different, louder
    /// failure the caller surfaces.
    pub fn check(&self, device: &DeviceId, verb: Verb) -> Result<(), LedgerError> {
        let record = self.record(device)?;
        match evaluate(record.as_ref(), verb) {
            Ok(()) => Ok(()),
            Err(denial) => Err(LedgerError::Refused(TrustRefusal::new(denial_message(
                &self.machine_name,
                device,
                verb,
                &denial,
            )))),
        }
    }

    /// Is there a live grant for this device at all? Used by the CLI to say
    /// whether a device is known here, without picking a verb.
    pub fn has_live_grant(&self, device: &DeviceId) -> Result<bool, crate::store::SessionError> {
        Ok(self.record(device)?.is_some_and(|record| record.is_live()))
    }

    /// One-time backfill: every device this machine already pinned gets the
    /// default grant, **once**.
    ///
    /// **Why this exists, and why it runs once.** Before T-0046 a paired device
    /// could use this machine; after it, a device with no grant row cannot. The
    /// rows do not exist for anything paired earlier, so without this every
    /// already-paired device would be locked out by an upgrade — and the failure
    /// would look like "the machine stopped trusting me", not like a migration.
    ///
    /// The marker is what keeps it honest in the other direction: if an operator
    /// later revokes every device, the ledger is legitimately empty, and a backfill
    /// that ran again would silently restore the access that was just taken away.
    /// So this runs once, ever, and says what it did.
    /// It reads the clock itself rather than taking a timestamp: a migration
    /// clock is not something a caller has an opinion about, and a parameter
    /// would invite each call site to source it differently.
    pub fn backfill_once(
        &self,
        existing: &[DeviceId],
    ) -> Result<Vec<DeviceId>, Box<dyn std::error::Error + Send + Sync>> {
        if self.store.trust_initialized()? {
            return Ok(Vec::new());
        }
        let now_ms = now_ms();
        let mut granted = Vec::new();
        for device in existing {
            // A device granting itself is exactly what the pairing default is:
            // there is no other device to name as the actor, and the pairing that
            // pinned it is what authorized it on this machine.
            self.grant(device, Role::Owner, device, now_ms)?;
            granted.push(device.clone());
        }
        self.store.mark_trust_initialized()?;
        Ok(granted)
    }
}

/// The current time, in epoch milliseconds. The ledger's own clock, for the one
/// operation (the migration) where the caller has no opinion about the time.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// What can go wrong when this machine is asked whether a device may do something.
///
/// The distinction is load-bearing: [`LedgerError::Refused`] is a decision, and
/// [`LedgerError::Store`] is a failure to decide. A caller that collapsed them
/// would treat an unreadable store as "no grant" and end sessions on a transient
/// SQLite error — or, worse in the other direction, treat it as a grant.
#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    /// The machine decided: no.
    #[error("{0}")]
    Refused(#[from] TrustRefusal),
    /// The machine could not decide.
    #[error("cannot read this machine's trust ledger: {0}")]
    Store(#[from] crate::store::SessionError),
}

/// A trust refusal, as an error type that carries the operator's message.
///
/// `VerbDenial`'s shape (T-0025) rather than a bare `String`, so a caller that
/// wants to match on the reason can, and one that only prints it does not have to.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct TrustRefusal {
    pub message: String,
}

impl TrustRefusal {
    #[must_use]
    pub fn new(message: String) -> Self {
        Self { message }
    }
}

/// The daemon's shared handle: one ledger, opened once, behind a mutex.
///
/// The session path checks a verb per message, so the ledger is opened once at
/// boot rather than per check — and the mutex is the store's own (SQLite
/// connections are not shareable), which is why this is a newtype rather than a
/// free function.
#[derive(Clone)]
pub struct SharedLedger(std::sync::Arc<Mutex<TrustLedger>>);

impl std::fmt::Debug for SharedLedger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("SharedLedger").finish_non_exhaustive()
    }
}

impl SharedLedger {
    #[must_use]
    pub fn new(ledger: TrustLedger) -> Self {
        Self(std::sync::Arc::new(Mutex::new(ledger)))
    }

    /// Run `f` against the ledger. Poisoning is recovered rather than propagated:
    /// a panicked *check* must not take the daemon's authorization down with it.
    pub fn with<T>(&self, f: impl FnOnce(&TrustLedger) -> T) -> T {
        match self.0.lock() {
            Ok(guard) => f(&guard),
            Err(poisoned) => f(&poisoned.into_inner()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger(tag: &str, name: &str) -> TrustLedger {
        let dir = std::env::temp_dir().join(format!(
            "arreo-trust-ledger-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        TrustLedger::open(
            &dir.join("session.db"),
            &dir.join("root.key"),
            name.to_string(),
        )
        .expect("ledger")
    }

    fn device(hex: char) -> DeviceId {
        DeviceId::parse(&hex.to_string().repeat(32)).expect("device id")
    }

    /// The machine's identity is its root key, and it does not move: a ledger
    /// reopened on the same files is the same machine.
    #[test]
    fn the_machine_identity_comes_from_its_root_key() {
        let dir = std::env::temp_dir().join(format!(
            "arreo-trust-identity-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        let db = dir.join("session.db");
        let key = dir.join("root.key");
        let first = TrustLedger::open(&db, &key, "pi".into()).expect("first");
        let again = TrustLedger::open(&db, &key, "pi".into()).expect("again");
        assert_eq!(first.machine(), again.machine());

        let other = TrustLedger::open(&dir.join("other.db"), &dir.join("other.key"), "vps".into())
            .expect("other");
        assert_ne!(
            first.machine(),
            other.machine(),
            "two machines must not share an identity"
        );
        assert_eq!(first.machine_name(), "pi");
    }

    /// The criterion, at the enforcement layer: a grant here is not a grant
    /// there, and the refusal is actionable.
    #[test]
    fn a_grant_here_is_not_a_grant_there_and_the_refusal_says_what_to_do() {
        let pi = ledger("isolate-pi", "pi");
        let phone = device('1');
        assert!(
            pi.check(&phone, Verb::Read).is_err(),
            "no grant yet: even reading is refused"
        );
        let message = pi
            .check(&phone, Verb::Read)
            .expect_err("no grant")
            .to_string();
        assert!(message.contains("pi"), "{message}");
        assert!(message.contains("arreo machines trust"), "{message}");
        assert!(message.contains("--role viewer"), "{message}");

        pi.grant(&phone, Role::Owner, &phone, 1_000).expect("grant");
        assert!(pi.check(&phone, Verb::Read).is_ok());
        assert!(pi.check(&phone, Verb::Spawn).is_ok());

        // A *different* machine's ledger knows nothing about that grant.
        let vps = ledger("isolate-vps", "vps");
        assert!(
            vps.devices().expect("rows").is_empty(),
            "the other machine has no rows at all"
        );
        let message = vps
            .check(&phone, Verb::Read)
            .expect_err("no grant")
            .to_string();
        assert!(message.contains("vps"), "{message}");
    }

    /// A viewer is refused control with a message naming the role it needs, and a
    /// revocation is immediate and reads differently.
    #[test]
    fn a_viewer_is_refused_control_and_a_revocation_reads_differently() {
        let pi = ledger("roles", "pi");
        let phone = device('2');
        pi.grant(&phone, Role::Viewer, &phone, 1_000)
            .expect("grant");

        assert!(pi.check(&phone, Verb::Read).is_ok(), "a viewer observes");
        assert!(pi.check(&phone, Verb::Metrics).is_ok());
        let message = pi
            .check(&phone, Verb::Spawn)
            .expect_err("a viewer may not spawn")
            .to_string();
        assert!(message.contains("owner"), "the role it needs: {message}");
        assert!(message.contains("--role owner"), "the command: {message}");

        assert!(pi.revoke(&phone, 2_000).expect("revoke"));
        assert!(
            pi.check(&phone, Verb::Read).is_err(),
            "a revoked grant observes nothing"
        );
        let message = pi
            .check(&phone, Verb::Read)
            .expect_err("revoked")
            .to_string();
        assert!(message.contains("revoked"), "{message}");
        assert!(message.contains("arreo machines trust"), "{message}");

        // Re-granting revives it: that is how an operator undoes a revocation.
        pi.grant(&phone, Role::Viewer, &phone, 3_000)
            .expect("re-grant");
        assert!(pi.check(&phone, Verb::Read).is_ok());
    }

    /// The backfill grants existing devices once, and never again — or a
    /// deliberate "revoke everything" would heal itself on the next restart.
    #[test]
    fn the_backfill_runs_once_and_does_not_undo_a_revocation() {
        let pi = ledger("backfill", "pi");
        let a = device('a');
        let b = device('b');

        let granted = pi.backfill_once(&[a.clone(), b.clone()]).expect("backfill");
        assert_eq!(granted.len(), 2);
        for device in [&a, &b] {
            assert!(pi.check(device, Verb::Spawn).is_ok(), "{device} works");
        }

        // A second call does nothing, and crucially does not resurrect rows.
        assert!(pi
            .backfill_once(&[a.clone(), b.clone()])
            .expect("second")
            .is_empty());

        pi.revoke(&a, 3_000).expect("revoke");
        assert!(pi.check(&a, Verb::Read).is_err());
        assert!(pi
            .backfill_once(&[a.clone(), b.clone()])
            .expect("third")
            .is_empty());
        assert!(
            pi.check(&a, Verb::Read).is_err(),
            "a revoked device must stay revoked across a restart, not be backfilled back"
        );
        assert!(
            pi.check(&b, Verb::Read).is_ok(),
            "and the other is untouched"
        );
    }

    /// The ledger's rows are this machine's only: `machine` is in the key.
    #[test]
    fn the_ledger_lists_only_its_own_rows() {
        let dir = std::env::temp_dir().join(format!(
            "arreo-trust-shared-store-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        let db = dir.join("session.db");
        let pi = TrustLedger::open(&db, &dir.join("pi.key"), "pi".into()).expect("pi");
        let vps = TrustLedger::open(&db, &dir.join("vps.key"), "vps".into()).expect("vps");
        assert_ne!(pi.machine(), vps.machine());

        let phone = device('c');
        pi.grant(&phone, Role::Owner, &phone, 1_000)
            .expect("pi grants");
        assert_eq!(pi.devices().expect("pi rows").len(), 1);
        assert!(
            vps.devices().expect("vps rows").is_empty(),
            "a shared store does not make a shared grant"
        );
        assert_eq!(
            pi.machine().as_str().len(),
            32,
            "the machine id is the bare hex the directory uses"
        );
    }
}
