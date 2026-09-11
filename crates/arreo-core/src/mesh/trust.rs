//! Per-machine device trust (T-0046, ROADMAP §3.7 and §4).
//!
//! One sentence: **a grant on machine A is not a grant on machine B**, and the
//! machinery that decides is a pure function over `(grant, verb)` so the daemon
//! enforces it, the CLI prints it, and the tests enumerate every cell.
//!
//! ## Why this is not the certificate's role
//!
//! A device certificate (ADR 0009) says what the *account* considers this device
//! to be. It travels with the device, is issued once at pairing, and is the same
//! on every machine the account owns. That is the wrong shape for §3.7's model:
//! a phone paired to the VPS must **not** be automatically trusted by the Pi, or
//! the first account device to touch a machine would own it.
//!
//! So there are two facts, and they are genuinely different:
//!
//! | Fact | Where it lives | Who decides |
//! | --- | --- | --- |
//! | the account's role for a device | the certificate, issued at pairing | the account root |
//! | **this machine's grant** to a device | the machine's own store | this machine |
//!
//! The second is what this module models. It reuses [`Role`] rather than
//! inventing a trust-specific role vocabulary, and it reuses
//! [`role::check`] rather than restating which verb needs which capability — a
//! second table of "what may a viewer do" is precisely the drift this project has
//! paid for before.
//!
//! ## The rule
//!
//! A device with no grant has no access — absence is refusal, never a default.
//! A revoked grant is a grant that is gone (the row stays, so the trail of "who
//! had this and when" survives). A live grant is evaluated by capability:
//! `read`/`metrics`/`wait`/`panes`/`attach`/`hello` need [`Capability::Observe`],
//! and `send`/`spawn`/`split`/`kill`/`admin` need [`Capability::Control`].

use crate::identity::role::{self, Capability, Verb};
use crate::identity::{DeviceId, Role};
use crate::mesh::MachineId;
use serde::{Deserialize, Serialize};

/// One machine's grant to one device.
///
/// Keyed `(machine_id, device_id)` everywhere it is stored: the pair is the
/// identity of the grant, and a store that allowed a row per device alone would
/// be the account-wide trust list §3.7 exists to avoid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustRecord {
    /// The machine that granted. From the granting machine's own point of view
    /// this is always itself; it travels in the record so a row exported from
    /// one machine cannot be mistaken for a row from another.
    pub machine_id: MachineId,
    pub device_id: DeviceId,
    pub role: Role,
    pub granted_at_ms: i64,
    /// The device that made the grant. Same as `device_id` when a device granted
    /// itself (the pairing default), and different when an operator extended
    /// trust to a second device — which is the case the audit trail exists for.
    pub granted_by: DeviceId,
    /// `None` while the grant is live. The row is never deleted: "who had access
    /// and when it stopped" is the question a revocation is asked to answer.
    pub revoked_at_ms: Option<i64>,
}

impl TrustRecord {
    /// Is this grant live right now?
    #[must_use]
    pub fn is_live(&self) -> bool {
        self.revoked_at_ms.is_none()
    }

    /// May this grant perform `verb`?
    pub fn permits(&self, verb: Verb) -> Result<(), TrustDenial> {
        if let Some(at_ms) = self.revoked_at_ms {
            return Err(TrustDenial::Revoked {
                device: self.device_id.clone(),
                machine: self.machine_id.clone(),
                at_ms,
            });
        }
        role::check(self.role, verb).map_err(|_| TrustDenial::InsufficientRole {
            device: self.device_id.clone(),
            machine: self.machine_id.clone(),
            role: self.role,
            needed: role::required(verb),
        })
    }
}

/// The rule, over an *optional* grant: no row at all is a refusal.
///
/// Taking `Option` rather than making the caller unwrap is the point — "the
/// device has no row here" and "the row says viewer" are different refusals with
/// different fixes, and a caller that collapsed them would print the wrong
/// command.
pub fn evaluate(record: Option<&TrustRecord>, verb: Verb) -> Result<(), TrustDenial> {
    match record {
        Some(record) => record.permits(verb),
        None => Err(TrustDenial::NoGrant { verb }),
    }
}

/// Why a device may not do something on a machine.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TrustDenial {
    /// The device has no grant on this machine at all.
    #[error("{verb:?} needs a grant this device does not have on this machine")]
    NoGrant { verb: Verb },
    #[error("this device's grant on this machine was revoked at {at_ms}")]
    Revoked {
        device: DeviceId,
        machine: MachineId,
        at_ms: i64,
    },
    /// The role is too low. The verb is deliberately *not* a field: this denial
    /// is about the role, the caller knows the verb it asked for, and a third
    /// copy of it here would be one more thing to keep in step.
    #[error("this device is {role} on this machine, which lacks {needed:?} privilege")]
    InsufficientRole {
        device: DeviceId,
        machine: MachineId,
        role: Role,
        needed: Capability,
    },
}

impl TrustDenial {
    /// The role the device would need for the refused verb.
    #[must_use]
    pub fn needed_role(verb: Verb) -> Role {
        match role::required(verb) {
            Capability::Observe => Role::Viewer,
            Capability::Control => Role::Owner,
        }
    }
}

/// The refusal an operator reads, with the command that fixes it.
///
/// **One builder, every refusal path.** T-0046 requires that a refusal name the
/// machine, the missing role and the exact granting command; a message assembled
/// at each call site is three chances to forget one of them, and the machine
/// that is *missing the command* is the one whose operator cannot act.
///
/// `machine` is the human name (a refusal that says `MachineId` is a refusal an
/// operator cannot act on), and `device` is the `dev_<hex>` display form — the
/// spelling the CLI parses back.
#[must_use]
pub fn denial_message(
    machine: &str,
    device: &DeviceId,
    verb: Verb,
    denial: &TrustDenial,
) -> String {
    let grant = format!(
        "arreo machines trust {} --machine {} --role {} --yes",
        device.display_id(),
        machine,
        TrustDenial::needed_role(verb).as_str()
    );
    match denial {
        TrustDenial::Revoked { .. } => format!(
            "machine {machine} revoked this device's grant, so {verb:?} is refused. \
             Re-grant it with: {grant}"
        ),
        TrustDenial::InsufficientRole { role, .. } => format!(
            "machine {machine} granted this device {role}, which may not {verb:?} \
             (it needs {}). Grant it with: {grant}",
            TrustDenial::needed_role(verb).as_str()
        ),
        TrustDenial::NoGrant { .. } => format!(
            "machine {machine} has no grant for this device, so {verb:?} is refused. \
             Grant it with: {grant}"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine() -> MachineId {
        MachineId::parse("22222222222222222222222222222222").expect("id")
    }

    fn device() -> DeviceId {
        DeviceId::parse("dev_11111111111111111111111111111111").expect("id")
    }

    fn grant(role: Role, revoked_at_ms: Option<i64>) -> TrustRecord {
        TrustRecord {
            machine_id: machine(),
            device_id: device(),
            role,
            granted_at_ms: 1_000,
            granted_by: device(),
            revoked_at_ms,
        }
    }

    /// Every cell of the role × verb matrix T-0046 asks for, asserted one by one
    /// — including the cells that must *succeed*, because a policy test that
    /// only checks refusals passes with a function that refuses everything.
    #[test]
    fn the_role_verb_matrix_is_asserted_cell_by_cell() {
        let observing = [
            Verb::Hello,
            Verb::Panes,
            Verb::Read,
            Verb::Attach,
            Verb::Wait,
            Verb::Metrics,
        ];
        let controlling = [
            Verb::Send,
            Verb::Spawn,
            Verb::Split,
            Verb::Kill,
            Verb::Admin,
        ];

        let viewer = grant(Role::Viewer, None);
        for verb in observing {
            assert!(
                viewer.permits(verb).is_ok(),
                "a viewer must observe: {verb:?} was refused"
            );
        }
        for verb in controlling {
            match viewer.permits(verb) {
                Err(TrustDenial::InsufficientRole { role, needed, .. }) => {
                    assert_eq!(role, Role::Viewer);
                    assert_eq!(needed, Capability::Control, "{verb:?}");
                }
                other => panic!("a viewer must not {verb:?}: {other:?}"),
            }
        }

        let operator = grant(Role::Owner, None);
        for verb in observing.into_iter().chain(controlling) {
            assert!(
                operator.permits(verb).is_ok(),
                "an operator must do everything v1 has: {verb:?} was refused"
            );
        }
    }

    /// No row is a refusal, and it is a *different* refusal from "your role is
    /// too low": one is fixed by granting, the other by re-granting with a role.
    #[test]
    fn no_grant_is_refused_and_says_so_differently() {
        let denial = evaluate(None, Verb::Read).expect_err("no grant");
        assert!(matches!(denial, TrustDenial::NoGrant { verb: Verb::Read }));

        let weak = evaluate(Some(&grant(Role::Viewer, None)), Verb::Send)
            .expect_err("a viewer may not send");
        assert!(matches!(weak, TrustDenial::InsufficientRole { .. }));
        assert_ne!(
            denial_message("pi", &device(), Verb::Read, &denial),
            denial_message("pi", &device(), Verb::Send, &weak),
            "two refusals with two fixes must not read the same"
        );
    }

    /// A revoked grant is gone, and the refusal says when it ended — a device
    /// whose access was cut needs to know it was deliberate.
    #[test]
    fn a_revoked_grant_is_refused_even_for_observing() {
        let revoked = grant(Role::Owner, Some(2_000));
        assert!(!revoked.is_live());
        for verb in [Verb::Read, Verb::Metrics, Verb::Send] {
            match revoked.permits(verb) {
                Err(TrustDenial::Revoked { at_ms, .. }) => assert_eq!(at_ms, 2_000),
                other => panic!("a revoked grant must refuse {verb:?}: {other:?}"),
            }
        }
    }

    /// The refusal names the machine, the missing role and the exact command —
    /// the three things T-0046 requires, asserted as substrings so the wording
    /// can improve without the contract breaking.
    #[test]
    fn a_denial_names_the_machine_the_role_and_the_command() {
        let viewer = grant(Role::Viewer, None);
        let denial = viewer.permits(Verb::Spawn).expect_err("viewer");
        let message = denial_message("the-pi", &device(), Verb::Spawn, &denial);
        assert!(message.contains("the-pi"), "the machine: {message}");
        assert!(message.contains("owner"), "the role it needs: {message}");
        assert!(
            message.contains("arreo machines trust dev_11111111111111111111111111111111"),
            "the exact command: {message}"
        );
        assert!(message.contains("--machine the-pi"), "{message}");
        assert!(message.contains("--yes"), "{message}");
        assert!(message.contains("Spawn"), "what was refused: {message}");

        // The same for a device with no row: the fix is to grant, and the
        // command must be there too.
        let none = evaluate(None, Verb::Read).expect_err("no grant");
        let message = denial_message("the-pi", &device(), Verb::Read, &none);
        assert!(message.contains("arreo machines trust"), "{message}");
        assert!(message.contains("--role viewer"), "{message}");
    }

    /// The needed role for a verb is derived from the same `required` table the
    /// enforcement uses — so a message can never recommend a role that would not
    /// work.
    #[test]
    fn the_recommended_role_is_the_one_that_works() {
        for verb in [
            Verb::Hello,
            Verb::Panes,
            Verb::Read,
            Verb::Attach,
            Verb::Wait,
            Verb::Metrics,
            Verb::Send,
            Verb::Spawn,
            Verb::Split,
            Verb::Kill,
            Verb::Admin,
        ] {
            let recommended = TrustDenial::needed_role(verb);
            let record = grant(recommended, None);
            assert!(
                record.permits(verb).is_ok(),
                "the role a denial recommends for {verb:?} must actually permit it"
            );
        }
    }

    /// The record is keyed by the pair, and the pair round-trips through the
    /// wire form the store and the CLI use.
    #[test]
    fn a_record_round_trips_and_carries_its_machine() {
        let record = grant(Role::Viewer, None);
        let json = serde_json::to_string(&record).expect("encode");
        let back: TrustRecord = serde_json::from_str(&json).expect("decode");
        assert_eq!(back, record);
        assert_eq!(back.machine_id, machine());
        assert_eq!(back.device_id, device());
        assert_ne!(
            back.machine_id,
            MachineId::parse("33333333333333333333333333333333").expect("id"),
            "the machine is part of the grant's identity, not decoration"
        );
    }
}
