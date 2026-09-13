//! Adapter registry (T-0072): one adapter per harness, selected by data.
//!
//! The daemon used to run every pane on the universal adapter
//! ([`Adapter::default`]); T-0072 makes the adapter a property of the program
//! being spawned, declared in `adapters/*.toml`: `pi` lands on `pi.toml`,
//! `opencode` on `opencode.toml`, and everything else on `default.toml`.
//! There is no per-harness branch anywhere in the Rust — the registry data
//! decides, exactly like the pattern lists do.
//!
//! The registry is compiled into the binary ([`AdapterRegistry::builtin`]),
//! the same way [`Adapter::default`] embeds `default.toml`, so a running
//! daemon needs no adapters directory and a restore can always resolve a
//! record's harness id.

use super::adapter::{Adapter, AdapterError};

/// The compiled-in registry: the universal adapter plus every harness adapter
/// in `adapters/*.toml`.
///
/// Contains no branching of its own — the three operations are the data:
/// [`AdapterRegistry::for_program`] maps a spawn to its adapter by the
/// programs each TOML declares, [`AdapterRegistry::by_harness`] resolves a
/// record's harness id back to the adapter that made it, and everything else
/// is the universal adapter.
#[derive(Debug, Clone)]
pub struct AdapterRegistry {
    default: Adapter,
    harnesses: Vec<Adapter>,
}

impl AdapterRegistry {
    /// Build a registry from the universal adapter plus every harness adapter.
    ///
    /// Validation is the loud half of "data decides": a harness id claimed by
    /// two adapters, or a program claimed by two, is an error — last-wins
    /// would be a silent lie about which strategy a pane got.
    pub fn new(default: Adapter, harnesses: Vec<Adapter>) -> Result<Self, AdapterError> {
        let mut seen_harnesses: Vec<&str> = Vec::new();
        let mut seen_programs: Vec<&str> = Vec::new();
        for adapter in &harnesses {
            let harness = adapter.harness_id().ok_or_else(|| {
                AdapterError::Invalid(
                    "a registry harness adapter must declare a harness id".to_string(),
                )
            })?;
            if seen_harnesses.contains(&harness) {
                return Err(AdapterError::Invalid(format!(
                    "harness {harness:?} is claimed by more than one adapter"
                )));
            }
            seen_harnesses.push(harness);
            for program in &adapter.programs {
                if seen_programs.contains(&program.as_str()) {
                    return Err(AdapterError::Invalid(format!(
                        "program {program:?} is claimed by more than one adapter"
                    )));
                }
                seen_programs.push(program);
            }
        }
        Ok(Self { default, harnesses })
    }

    /// The compiled-in registry, parsed once and shared: immutable data that
    /// ships in the binary. Parses and validates never fail (the fixtures
    /// `xtask adapters --check` and the unit tests pin them); when they do,
    /// that is a build-time mistake that must not run.
    #[must_use]
    pub fn builtin() -> &'static AdapterRegistry {
        static BUILTIN: std::sync::LazyLock<AdapterRegistry> = std::sync::LazyLock::new(|| {
            AdapterRegistry::new(
                Adapter::default(),
                vec![
                    Adapter::from_toml(include_str!("../../../../adapters/pi.toml"))
                        .expect("pi adapter is valid (T-0017 fixtures pin it)"),
                    Adapter::from_toml(include_str!("../../../../adapters/opencode.toml"))
                        .expect("opencode adapter is valid (T-0017 fixtures pin it)"),
                    Adapter::from_toml(include_str!("../../../../adapters/omp.toml"))
                        .expect("omp adapter is valid (T-0080 fixtures pin it)"),
                ],
            )
            .expect("builtin adapters are valid and disjoint (xtask adapters --check)")
        });
        &BUILTIN
    }

    /// The universal adapter: every program no harness adapter claims.
    #[must_use]
    pub fn default(&self) -> &Adapter {
        &self.default
    }

    /// The adapter for a program about to be spawned: the first harness
    /// adapter whose declared program matches `argv[0]`'s basename — a path
    /// (`/home/me/bin/pi`) matches by its file name (`pi`) — else the
    /// universal adapter.
    #[must_use]
    pub fn for_program(&self, program: &str) -> &Adapter {
        let base = std::path::Path::new(program)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(program);
        self.harnesses
            .iter()
            .find(|adapter| adapter.programs.iter().any(|declared| declared == base))
            .unwrap_or(&self.default)
    }

    /// The adapter behind a record's stored harness id, if the registry still
    /// knows it. A record whose harness was removed from the registry (a
    /// downgrade, a renamed adapter) is restored the pre-T-0072 way: the
    /// caller's fallback, loudly — never a strategy the data no longer owns.
    #[must_use]
    pub fn by_harness(&self, harness: &str) -> Option<&Adapter> {
        self.harnesses
            .iter()
            .find(|adapter| adapter.harness_id() == Some(harness))
    }

    /// The adapter a restore should resume a record with: by stored harness id
    /// when the record has one, else by the record's program (a pane spawned
    /// on an adapter whose harness was never recorded — pre-v8 rows).
    #[must_use]
    pub fn for_record(&self, harness: Option<&str>, program: &str) -> &Adapter {
        harness
            .and_then(|id| self.by_harness(id))
            .unwrap_or_else(|| self.for_program(program))
    }
}

/// A fresh session id for the `pin` strategy: a v4-shaped UUID, the form both
/// harnesses expect a caller-chosen session to take. Generated from OS
/// entropy, not the clock — a clock-seeded id is guessable, and a session id
/// the caller can predict is a session id another process can claim.
#[must_use]
pub fn new_session_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("OS entropy for a pinned session id");
    // RFC 4122 v4 bits: variant 10xx, version 0100.
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::adapter::Adapter;

    fn pi_like() -> Adapter {
        Adapter::from_toml(
            "idle_after_ms = 2000\nquestion_after_ms = 2000\nblocked_after_ms = 2500\n\
             question_patterns = ['x']\nerror_patterns = ['y']\n\
             harness = 'pi'\nprograms = ['pi']\n\
             [resume]\nkind = 'pin'\nargv = ['--session-id', '{session}']\n\
             session_pattern = '\"type\":\"session\".*?\"id\":\"([0-9a-f-]{36})\"'",
        )
        .expect("pi-like adapter is valid")
    }

    fn continue_like() -> Adapter {
        Adapter::from_toml(
            "idle_after_ms = 2000\nquestion_after_ms = 2000\nblocked_after_ms = 2500\n\
             question_patterns = ['x']\nerror_patterns = ['y']\n\
             harness = 'opencode'\nprograms = ['opencode']\n\
             [resume]\nkind = 'continue'\nargv = ['--continue']\n\
             exact_argv = ['--session', '{session}']\n\
             session_pattern = '\"sessionID\":\"(ses_[A-Za-z0-9]+)\"'",
        )
        .expect("continue-like adapter is valid")
    }

    fn base_adapter_toml() -> String {
        "idle_after_ms = 2000\nquestion_after_ms = 2000\nblocked_after_ms = 2500\n\
         question_patterns = ['x']\nerror_patterns = ['y']\n"
            .to_string()
    }

    #[test]
    fn registry_maps_program_basenames_and_falls_back() {
        let registry = AdapterRegistry::new(Adapter::default(), vec![pi_like(), continue_like()])
            .expect("disjoint");
        assert_eq!(
            registry.for_program("/home/me/bin/pi").harness_id(),
            Some("pi")
        );
        assert_eq!(
            registry.for_program("opencode").harness_id(),
            Some("opencode")
        );
        assert_eq!(registry.for_program("vim").harness_id(), None);
        assert_eq!(
            registry.for_record(Some("pi"), "/opt/pi").harness_id(),
            Some("pi")
        );
        assert_eq!(registry.for_record(None, "vim").harness_id(), None);
        assert!(registry.by_harness("ghost").is_none());
    }

    #[test]
    fn registry_rejects_duplicate_claims() {
        let dup_program = Adapter::from_toml(&format!(
            "{}harness = 'other'\nprograms = ['pi']\n",
            base_adapter_toml()
        ))
        .expect("valid alone");
        assert!(AdapterRegistry::new(Adapter::default(), vec![pi_like(), dup_program]).is_err());
        let dup_harness = Adapter::from_toml(&format!(
            "{}harness = 'pi'\nprograms = ['pi2']\n",
            base_adapter_toml()
        ))
        .expect("valid alone");
        assert!(AdapterRegistry::new(Adapter::default(), vec![pi_like(), dup_harness]).is_err());
    }

    #[test]
    fn new_session_id_is_a_v4_shaped_uuid_and_unique() {
        let a = new_session_id();
        let b = new_session_id();
        assert_ne!(a, b);
        let parts: Vec<&str> = a.split('-').collect();
        assert_eq!(parts.len(), 5);
        assert_eq!(parts[0].len(), 8);
        assert_eq!(parts[1].len(), 4);
        assert_eq!(parts[3].len(), 4);
        assert_eq!(parts[4].len(), 12);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit() || c == '-'));
    }
}
