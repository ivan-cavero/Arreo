//! The revocation decision (T-0026, ROADMAP §3.3/§3.14/§4).
//!
//! One sentence: **may this device talk to this machine**, answered in one place,
//! so the handshake, the pairing flow and the authorization check cannot disagree
//! about a device that has been cut off.
//!
//! **Why this is a module and not three `if` statements.** Revocation was already
//! decided in two places — `DeviceIndex::authorize` (which the per-verb gate and
//! the CLI go through) and the transport's pinned-key resolver (which the
//! handshake goes through) — and those two answers had already drifted once in
//! this codebase's short life, because each spelled the rule out for itself. The
//! rule is three facts (`revoked`, `retired_to`, and "the certificate must
//! verify"), and three facts stated twice is two chances to be wrong.
//!
//! **What revocation is not.** It is not expiry: nothing here consults a clock,
//! and no device becomes unauthorized by age (§3.14 "nothing expires by age").
//! It is not a network check: the list is local SQLite, so a machine with its
//! relay down still refuses a stolen phone. And it is not deletion: a revoked
//! device keeps its row as a tombstone, so un-revoking is not something that can
//! happen by accident (a fresh pin and a full re-pairing are required).
//!
//! **What it deliberately does not cover.** A session that is *already open* is
//! T-0052's problem: this module answers a question, it does not reach into a
//! running stream. And propagating the list to other machines and the relay is
//! the mesh work (T-0046) — the relay verifies a certificate chain and, until
//! then, has no list to consult.

use crate::identity::{DeviceId, DeviceRecord, Role};

/// Who revoked a device when no device did: an operator acting on this machine's
/// own Unix socket. A stable string rather than a device id, because the admin
/// may not *have* a device identity — and because an audit row that says
/// "local-cli" is more useful than one that says nothing.
pub const LOCAL_CLI: &str = "local-cli";

/// The reason a device may not connect, when it may not.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Denied {
    #[error("device {device} was revoked")]
    Revoked { device: DeviceId },
    #[error("device {device} was rotated away to {replaced_by}")]
    RotatedAway {
        device: DeviceId,
        replaced_by: DeviceId,
    },
}

impl Denied {
    /// The stable label an audit row carries for this refusal. Kept here so the
    /// audit vocabulary and the decision cannot drift either.
    #[must_use]
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Revoked { .. } => "revoked",
            Self::RotatedAway { .. } => "rotated away",
        }
    }
}

/// May this device connect?
///
/// The order matters for the message a human reads: a rotated device is *also*
/// not revoked, and saying "rotated away to X" is far more useful than "not
/// pinned" — which is what a lookup would otherwise report for a key the
/// operator deliberately moved forward.
pub fn may_connect(record: &DeviceRecord) -> Result<(), Denied> {
    if record.revoked {
        return Err(Denied::Revoked {
            device: record.id.clone(),
        });
    }
    if let Some(replacement) = &record.retired_to {
        return Err(Denied::RotatedAway {
            device: record.id.clone(),
            replaced_by: replacement.clone(),
        });
    }
    Ok(())
}

/// Whether a role may be granted at all. A revoked device is never re-pinned:
/// un-revoking requires a new key, which is what "a burned key is never silently
/// restored" means in code.
pub fn may_pin(record: Option<&DeviceRecord>) -> Result<(), Denied> {
    match record {
        Some(record) => may_connect(record),
        None => Ok(()),
    }
}

/// The role a device holds if it may connect, or the reason it may not.
///
/// The shape both callers actually want: "who is this and what may they do" is
/// one question at the handshake, and answering it in two steps is how a caller
/// ends up checking authorization without checking revocation.
pub fn authorized_role(record: &DeviceRecord) -> Result<Role, Denied> {
    may_connect(record).map(|()| record.role)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::cert::{DeviceCert, FIRST_SERIAL};
    use crate::identity::keys::{DeviceKey, RootKey};

    fn record(revoked: bool, retired_to: Option<&str>) -> DeviceRecord {
        let key = DeviceKey::from_seed([7u8; 32]);
        let root = RootKey::generate().expect("entropy");
        let cert = DeviceCert::issue(
            &root,
            &key.public(),
            "phone",
            Role::Owner,
            1_000,
            FIRST_SERIAL,
        );
        let mut record = DeviceRecord::from_cert(&cert);
        record.revoked = revoked;
        record.retired_to = retired_to.map(|id| DeviceId::parse(id).expect("id"));
        record
    }

    #[test]
    fn a_live_device_may_connect_and_keeps_its_role() {
        let live = record(false, None);
        assert!(may_connect(&live).is_ok());
        assert_eq!(authorized_role(&live).expect("allowed"), Role::Owner);
    }

    #[test]
    fn a_revoked_device_is_refused_by_name() {
        let revoked = record(true, None);
        let denied = may_connect(&revoked).expect_err("a revoked device is refused");
        assert!(matches!(denied, Denied::Revoked { .. }));
        assert_eq!(denied.reason(), "revoked");
        assert!(
            denied.to_string().contains(&revoked.id.display_id()),
            "the refusal names the device: {denied}"
        );
    }

    #[test]
    fn a_rotated_device_is_told_where_it_went() {
        let rotated = record(false, Some("11111111111111111111111111111111"));
        let denied = may_connect(&rotated).expect_err("a rotated device is refused");
        match denied {
            Denied::RotatedAway { replaced_by, .. } => {
                assert_eq!(replaced_by.as_str(), "11111111111111111111111111111111");
            }
            other => panic!("expected a rotation refusal, got {other:?}"),
        }
        assert_eq!(
            may_connect(&rotated).expect_err("still refused").reason(),
            "rotated away"
        );
    }

    /// Revocation outranks rotation in the message: a device that was revoked
    /// *and* rotated is reported as revoked, because that is the fact that
    /// matters to whoever is reading the log.
    #[test]
    fn revocation_is_reported_ahead_of_rotation() {
        let both = record(true, Some("11111111111111111111111111111111"));
        assert!(matches!(may_connect(&both), Err(Denied::Revoked { .. })));
    }

    /// The pinning door: a revoked or rotated device is never re-pinned, which is
    /// what makes "a burned key is never silently restored" true rather than
    /// aspirational.
    #[test]
    fn a_burned_key_is_never_re_pinned() {
        assert!(may_pin(None).is_ok(), "a fresh key may be pinned");
        assert!(may_pin(Some(&record(false, None))).is_ok());
        assert!(matches!(
            may_pin(Some(&record(true, None))),
            Err(Denied::Revoked { .. })
        ));
        assert!(matches!(
            may_pin(Some(&record(
                false,
                Some("11111111111111111111111111111111")
            ))),
            Err(Denied::RotatedAway { .. })
        ));
    }
}
