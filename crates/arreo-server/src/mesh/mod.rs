//! The mesh: what this machine knows about the account's machines and the
//! devices allowed to touch it (T-0046, ROADMAP §3.7).
//!
//! The directory (names, presence, ids) lives at the relay and is mirrored by
//! `arreo_core::mesh::directory`. What lives *here* is the machine-local half:
//! the trust ledger, which is this machine's own decision about who may use it
//! and is deliberately not shared with the account.

pub mod trust;

pub use trust::{GrantedDevice, LedgerError, SharedLedger, TrustLedger, TrustRefusal};
