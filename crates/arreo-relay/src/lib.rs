//! Arreo relay library: the pairing mailbox today, routing and presence next.
//!
//! T-0024 ships the **pairing mailbox** ([`pairing`]): the one part of the
//! relay the pairing flow depends on, with the rules that make a 32-bit code
//! safe (single-use sessions, write-once slots, a bounded lifetime).
//!
//! Routing, the durable per-device inbox and presence are the relay v0 work
//! (T-0029..T-0031); they extend this crate, and they must not weaken the
//! guarantees here (see the module docs for the exact rules).

pub mod directory;
pub mod inbox;
pub mod pairing;
pub mod presence;
pub mod router;
pub mod store;

pub use directory::{clock_offset_ms, Directory, DirectoryFailure, JoinTicket, CLOCK_OFFSET_ENV};
pub use inbox::{
    Drained, Enqueued, Inbox, InboxError, InboxLimits, InboxStats, DEFAULT_MAX_MB,
    DEFAULT_MAX_MESSAGES, DEFAULT_TTL_DAYS,
};
pub use pairing::{Mailbox, MAX_FRAME_BYTES, MAX_REMEMBERED_BURNS, READ_TIMEOUT};
pub use presence::{
    format_age, missed_beats, next_heartbeat_delay, presence_at, DevicePresence,
    HEARTBEAT_INTERVAL, HEARTBEAT_JITTER,
};
pub use router::{Router, RouterError, Session};
pub use store::{RelayStore, StoreError, SCHEMA_VERSION};
