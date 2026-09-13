//! Version vectors (T-0083): who has seen what, per file, with no authority.
//!
//! One sentence: each machine counts its own revisions of a file, the counter
//! map is the file's vector, and comparing two vectors says — without asking
//! anyone — whether one side's copy is newer, older, or *concurrently* edited.
//!
//! Why a vector and not a timestamp or a hash: ROADMAP §3.8's case is four PCs
//! with no central authority, and two of them can edit while the third is
//! offline. A timestamp makes the slower clock win; a content hash can say the
//! bytes differ but not which is newer; and "newest write wins" is exactly the
//! behaviour the conflict rule forbids. A vector answers the only question the
//! receiver has — did the sender see everything I have? — and it answers it from
//! data the receiver already holds.
//!
//! The transport is T-0086's; this is the arithmetic it will carry, tested where
//! it lives so a protocol change cannot quietly change what "concurrent" means.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// How one file's two copies relate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    /// Both sides have seen the same revisions.
    Same,
    /// `self` has seen everything `other` has, and something more.
    Newer,
    /// `other` has seen everything `self` has, and something more.
    Older,
    /// Each side has a revision the other has not seen: a real conflict, which
    /// is kept as two files rather than resolved by the machine that happens to
    /// run last.
    Concurrent,
}

/// A file's revision counters, one per machine that ever pushed it.
///
/// Ordered by machine name (`BTreeMap`) so two vectors that mean the same thing
/// serialise to the same bytes — the payload is compared and hashed, and an
/// order that depended on insertion would make equal vectors look different.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vector(BTreeMap<String, u64>);

impl Vector {
    #[must_use]
    pub fn new() -> Self {
        Self(BTreeMap::new())
    }

    /// `self` built from pairs, for tests and for the store's rows.
    #[must_use]
    pub fn from_pairs<I: IntoIterator<Item = (String, u64)>>(pairs: I) -> Self {
        Self(pairs.into_iter().collect())
    }

    /// The counter `machine` is at; a machine that never pushed is at 0, which
    /// is what makes an empty vector compare correctly against a full one.
    #[must_use]
    pub fn get(&self, machine: &str) -> u64 {
        self.0.get(machine).copied().unwrap_or(0)
    }

    /// Count `machine`'s next revision and return its number.
    pub fn bump(&mut self, machine: &str) -> u64 {
        let next = self.get(machine).saturating_add(1);
        self.0.insert(machine.to_string(), next);
        next
    }

    /// Record `machine` at `counter` (a received payload's own count).
    pub fn set(&mut self, machine: &str, counter: u64) {
        self.0.insert(machine.to_string(), counter);
    }

    /// Take the pointwise maximum of `other`, returning whether anything moved.
    ///
    /// Pointwise max is the merge of two knowledge sets: after applying the
    /// peer's payload, this machine has seen everything both of them had, and
    /// the peer's counter for its own machine is adopted verbatim because the
    /// peer is the only authority on its own count.
    pub fn absorb(&mut self, other: &Vector) -> bool {
        let mut changed = false;
        for (machine, counter) in &other.0 {
            if self.get(machine) < *counter {
                self.0.insert(machine.clone(), *counter);
                changed = true;
            }
        }
        changed
    }

    /// Does `self` know everything `other` knows, and strictly more?
    #[must_use]
    pub fn dominates(&self, other: &Vector) -> bool {
        let mut strictly = false;
        for machine in self.0.keys().chain(other.0.keys()) {
            let mine = self.get(machine);
            let theirs = other.get(machine);
            if mine < theirs {
                return false;
            }
            if mine > theirs {
                strictly = true;
            }
        }
        strictly
    }

    /// How `self` relates to `other`.
    #[must_use]
    pub fn relation(&self, other: &Vector) -> Relation {
        if self == other {
            return Relation::Same;
        }
        if self.dominates(other) {
            Relation::Newer
        } else if other.dominates(self) {
            Relation::Older
        } else {
            Relation::Concurrent
        }
    }

    /// Is this the vector of a file no machine has ever pushed?
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// `machine:counter` pairs, for a one-line report.
    #[must_use]
    pub fn summary(&self) -> String {
        if self.0.is_empty() {
            return "-".to_string();
        }
        self.0
            .iter()
            .map(|(machine, counter)| format!("{machine}:{counter}"))
            .collect::<Vec<_>>()
            .join(",")
    }

    /// The pairs, for the store's rows.
    pub fn pairs(&self) -> impl Iterator<Item = (&str, u64)> {
        self.0
            .iter()
            .map(|(machine, counter)| (machine.as_str(), *counter))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vector(pairs: &[(&str, u64)]) -> Vector {
        Vector::from_pairs(pairs.iter().map(|(m, c)| ((*m).to_string(), *c)))
    }

    #[test]
    fn the_relation_names_the_three_real_cases() {
        let a1 = vector(&[("alpha", 1)]);
        let a2 = vector(&[("alpha", 2)]);
        assert_eq!(a1.relation(&a1.clone()), Relation::Same);
        assert_eq!(a2.relation(&a1), Relation::Newer);
        assert_eq!(a1.relation(&a2), Relation::Older);
        // Two machines that each edited without seeing the other: neither is
        // newer, which is the only reading that keeps both copies.
        let b1 = vector(&[("beta", 1)]);
        assert_eq!(a1.relation(&b1), Relation::Concurrent);
        assert_eq!(b1.relation(&a1), Relation::Concurrent);
        // A machine that has seen both is newer than either.
        let both = vector(&[("alpha", 1), ("beta", 1)]);
        assert_eq!(both.relation(&a1), Relation::Newer);
        assert_eq!(both.relation(&b1), Relation::Newer);
    }

    #[test]
    fn a_file_nobody_pushed_is_older_than_every_payload() {
        let empty = Vector::new();
        let pushed = vector(&[("alpha", 1)]);
        assert_eq!(empty.relation(&pushed), Relation::Older);
        assert_eq!(pushed.relation(&empty), Relation::Newer);
        assert_eq!(empty.relation(&empty.clone()), Relation::Same);
        assert!(empty.is_empty());
    }

    #[test]
    fn absorbing_a_peer_keeps_both_machines_counters_and_their_own_count() {
        let mut mine = vector(&[("beta", 3)]);
        let theirs = vector(&[("alpha", 7), ("beta", 1)]);
        assert!(mine.absorb(&theirs));
        assert_eq!(
            mine.get("alpha"),
            7,
            "the peer's count for itself is adopted"
        );
        assert_eq!(mine.get("beta"), 3, "my own count is never lowered");
        // Idempotent: absorbing the same payload twice is not a second change.
        assert!(!mine.absorb(&theirs));
        assert_eq!(mine.summary(), "alpha:7,beta:3");
    }

    #[test]
    fn a_bump_counts_only_this_machine() {
        let mut mine = vector(&[("alpha", 4)]);
        assert_eq!(mine.bump("alpha"), 5);
        assert_eq!(mine.bump("beta"), 1);
        assert_eq!(mine.summary(), "alpha:5,beta:1");
    }
}
