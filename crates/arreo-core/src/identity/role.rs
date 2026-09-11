//! Roles and the verb policy (T-0025, ROADMAP §4).
//!
//! One sentence: v1 has two roles — `owner` and `viewer` — and every socket
//! verb maps to exactly one capability, so "what may this device do" has a
//! single answer in one place instead of a scattering of checks.
//!
//! The policy is deliberately a pure function over (role, verb): the daemon
//! calls it, the CLI prints it, and the tests enumerate it. A verb with no
//! mapping is a compile-time omission, not a silent allow.

use serde::{Deserialize, Serialize};

/// v1 roles (§4: owner + viewer).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Full control of the machine's agents: everything a viewer may do, plus
    /// driving them (send, spawn, split).
    Owner,
    /// Read-only: watch panes, read scrollback, wait for states, sample metrics.
    Viewer,
}

impl Role {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Viewer => "viewer",
        }
    }

    /// Parse a role name.
    ///
    /// **Two spellings, one value.** ROADMAP §4 names the roles
    /// `viewer / operator / admin` and §3.7 says "roles (viewer/operator)
    /// evaluated on the machine that owns the agents", while the certificate
    /// this type was born with calls the same thing `owner` (ADR 0009). Accepting
    /// both here is deliberate: the alternative is a second role type whose only
    /// job is to be translated into this one, and a translation table is the
    /// "one fact, two spellings" defect with a place to hide. `operator` is the
    /// roadmap's word for what this value has always *meant* — holder of
    /// [`Capability::Control`] — so both spellings parse to it and `as_str`
    /// keeps printing `owner`, which is what the certificates on disk say.
    pub fn parse(text: &str) -> Result<Self, RoleError> {
        match text.trim().to_ascii_lowercase().as_str() {
            "owner" | "operator" => Ok(Self::Owner),
            "viewer" => Ok(Self::Viewer),
            other => Err(RoleError::Unknown(other.to_string())),
        }
    }

    /// Does this role hold `capability`? Owner is a superset by construction —
    /// stated once here rather than repeated at every call site.
    #[must_use]
    pub fn allows(self, capability: Capability) -> bool {
        match capability {
            Capability::Observe => true,
            Capability::Control => self == Self::Owner,
        }
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a verb needs. Two capabilities cover v1; more can be added when a verb
/// genuinely differs (and the exhaustive match in [`required`] will force the
/// decision).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    /// Watching: the agent's output and state are the user's own work product.
    Observe,
    /// Driving: sending input or creating panes changes someone's machine.
    Control,
}

/// The socket verbs, as the policy sees them. Kept as a separate enum from the
/// wire `Message` so the policy can be exhaustively matched and unit-tested
/// without constructing protocol values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Hello,
    Panes,
    Read,
    Attach,
    Wait,
    Metrics,
    Send,
    Spawn,
    Split,
    Kill,
    Admin,
}

/// The capability a verb requires.
///
/// Adding a verb without adding it here does not compile — the point of an
/// exhaustive match on a security boundary.
#[must_use]
pub fn required(verb: Verb) -> Capability {
    match verb {
        // Observing the machine the device is paired with.
        Verb::Hello | Verb::Panes | Verb::Read | Verb::Attach | Verb::Wait | Verb::Metrics => {
            Capability::Observe
        }
        // Changing it.
        Verb::Send | Verb::Spawn | Verb::Split | Verb::Kill | Verb::Admin => Capability::Control,
    }
}

/// The authorization answer, with the reason on refusal.
pub fn check(role: Role, verb: Verb) -> Result<(), RoleError> {
    let capability = required(verb);
    if role.allows(capability) {
        return Ok(());
    }
    Err(RoleError::Denied {
        role,
        verb,
        capability,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RoleError {
    #[error("unknown role {0:?} (want owner or viewer)")]
    Unknown(String),
    #[error("{role} may not {verb:?} (needs {capability:?})")]
    Denied {
        role: Role,
        verb: Verb,
        capability: Capability,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every verb, so the policy table cannot silently grow a hole.
    const ALL_VERBS: &[Verb] = &[
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
    ];

    #[test]
    fn a_viewer_may_observe_and_only_observe() {
        // The acceptance criterion, verb by verb.
        for verb in [
            Verb::Attach,
            Verb::Read,
            Verb::Wait,
            Verb::Panes,
            Verb::Metrics,
        ] {
            check(Role::Viewer, verb).unwrap_or_else(|e| panic!("viewer must {verb:?}: {e}"));
        }
        for verb in [
            Verb::Send,
            Verb::Spawn,
            Verb::Split,
            Verb::Kill,
            Verb::Admin,
        ] {
            assert!(
                matches!(check(Role::Viewer, verb), Err(RoleError::Denied { .. })),
                "viewer must not {verb:?}"
            );
        }
    }

    #[test]
    fn an_owner_may_do_everything_the_protocol_has() {
        for verb in ALL_VERBS {
            check(Role::Owner, *verb).unwrap_or_else(|e| panic!("owner must {verb:?}: {e}"));
        }
    }

    #[test]
    fn the_denial_says_what_was_needed() {
        match check(Role::Viewer, Verb::Send) {
            Err(RoleError::Denied {
                role,
                verb,
                capability,
            }) => {
                assert_eq!(role, Role::Viewer);
                assert_eq!(verb, Verb::Send);
                assert_eq!(capability, Capability::Control);
            }
            other => panic!("expected a typed denial, got {other:?}"),
        }
    }

    #[test]
    fn roles_parse_strictly() {
        assert_eq!(Role::parse("owner"), Ok(Role::Owner));
        assert_eq!(Role::parse(" VIEWER "), Ok(Role::Viewer));
        // The roadmap's word for the same role parses to the same value: one
        // fact, one value, two spellings accepted at the door (§4 vs ADR 0009).
        assert_eq!(
            Role::parse("operator").expect("the roadmap's word"),
            Role::Owner
        );
        assert_eq!(
            Role::parse("  OPERATOR ").expect("trimmed and cased"),
            Role::Owner
        );
        assert_eq!(Role::parse("owner"), Role::parse("operator"));
        assert!(
            matches!(Role::parse("admin"), Err(RoleError::Unknown(_))),
            "admin is a Team-tier role (ROADMAP §4); v1 does not have it"
        );
        assert!(matches!(Role::parse(""), Err(RoleError::Unknown(_))));
    }

    #[test]
    fn owner_is_a_superset_of_viewer() {
        for verb in ALL_VERBS {
            if check(Role::Viewer, *verb).is_ok() {
                check(Role::Owner, *verb).expect("owner must not be narrower than viewer");
            }
        }
    }
}
