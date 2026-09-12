//! The durable per-device inbox (T-0030, ROADMAP §3.14).
//!
//! One sentence: an envelope for a device that is not connected is committed to
//! SQLite before its sender is told `queued`, drained in `seq` order when the
//! device returns, and — when a bound forces a drop — counted rather than
//! vanishing.
//!
//! **Why the relay holds the queue.** The sender may be a phone that is about to
//! sleep, and mobile operating systems kill background sockets, so a
//! sender-side retry cannot outlive the sender and a machine-side queue cannot
//! exist while the machine is off. The queue has to live at the one party that
//! is always up, and it has to be on disk: "power the server off for two weeks
//! and everything resumes" is a durability claim, not a buffering one.
//!
//! **What the relay can see.** The payload is a `BLOB` and nothing here
//! interprets it — the rows are `(device, seq, received_at, expires_at, bytes)`
//! and a schema test fails if a column could hold a key, a pairing code or agent
//! state. Confidentiality is the daemons' Noise session (T-0023); the relay
//! stores what it cannot read, which is exactly the §4 promise.
//!
//! **Exactly-once, stated precisely.** The wire is *at-least-once*: a row is
//! deleted only when the consumer acks it, so a disconnect mid-drain redelivers.
//! What makes the consumer see each message once is the pair (monotonic `seq`
//! per destination, ack advancing the cursor in one transaction) plus the
//! consumer's own `(device, seq)` dedupe — the same shape the daemon's
//! `Read{from_line}` cursor already has (ADR 0007). Claiming end-to-end
//! exactly-once without the consumer's half would be a lie.

use crate::store::{RelayStore, StoreError};
use arreo_core::store::AuditOutcome;
use rusqlite::{params, OptionalExtension, TransactionBehavior};

/// Default retention: 30 days (§3.14). The managed tiers are pricing
/// configuration of this same knob, never a fork — self-hosted gets parity.
pub const DEFAULT_TTL_DAYS: u64 = 30;

/// Default per-device bound: 10,000 messages.
pub const DEFAULT_MAX_MESSAGES: u64 = 10_000;

/// Default per-device bound: 64 MiB.
pub const DEFAULT_MAX_MB: u64 = 64;

/// How many messages one drain may return, so a device returning after a month
/// cannot be handed an unbounded burst in a single frame.
pub const DEFAULT_DRAIN_LIMIT: usize = 256;

/// The retention window, in milliseconds.
#[must_use]
pub fn ttl_ms(days: u64) -> i64 {
    (days as i64) * 24 * 60 * 60 * 1000
}

/// The bounds an inbox enforces, all of them explicit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InboxLimits {
    /// Longest a message may wait before it is expired.
    pub ttl_ms: i64,
    /// Most messages one device may have queued.
    pub max_messages: u64,
    /// Most bytes one device may have queued.
    pub max_bytes: u64,
}

impl Default for InboxLimits {
    fn default() -> Self {
        Self {
            ttl_ms: ttl_ms(DEFAULT_TTL_DAYS),
            max_messages: DEFAULT_MAX_MESSAGES,
            max_bytes: DEFAULT_MAX_MB * 1024 * 1024,
        }
    }
}

impl InboxLimits {
    /// Build from the operator's flags, refusing values that make no sense.
    pub fn from_options(ttl_days: u64, max_messages: u64, max_mb: u64) -> Result<Self, InboxError> {
        if !(1..=365).contains(&ttl_days) {
            return Err(InboxError::BadLimit(format!(
                "inbox TTL must be 1..=365 days, got {ttl_days}"
            )));
        }
        if max_messages == 0 {
            return Err(InboxError::BadLimit(
                "inbox message bound must be at least 1".to_string(),
            ));
        }
        if max_mb == 0 {
            return Err(InboxError::BadLimit(
                "inbox byte bound must be at least 1 MiB".to_string(),
            ));
        }
        Ok(Self {
            ttl_ms: ttl_ms(ttl_days),
            max_messages,
            max_bytes: max_mb * 1024 * 1024,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum InboxError {
    #[error("inbox store: {0}")]
    Store(#[from] StoreError),
    #[error("inbox sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("{0}")]
    BadLimit(String),
    /// The message alone is larger than the whole per-device budget. Refusing it
    /// is the only bounded answer: evicting everything else to make room for one
    /// oversized message would turn a big send into a denial of service on the
    /// device's own queue.
    #[error("message of {size} bytes exceeds the {bound}-byte inbox budget for {device}")]
    TooLarge {
        device: String,
        size: u64,
        bound: u64,
    },
}

/// What one enqueue did, so the sender can be told the truth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enqueued {
    pub seq: u64,
    /// Messages queued for this device now.
    pub queued: u64,
    /// Messages evicted by *this* call to stay inside the bound.
    pub evicted: Vec<u64>,
    /// Drops this device has accumulated in total (evictions + expiries).
    pub dropped_total: u64,
}

/// What a drain returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drained {
    /// `(seq, payload)` in ascending `seq` order — the stored bytes, which for a
    /// routed envelope is the whole framed message (see [`Inbox::enqueue`]).
    pub messages: Vec<(u64, Vec<u8>)>,
    /// The same rows as complete frames, ready to write to the peer without the
    /// relay decoding anything.
    pub raw: Vec<Vec<u8>>,
    /// How many messages were dropped since the previous drain — reported, so a
    /// drop is never silent — and reset by this drain.
    pub dropped: u64,
    /// How many expired during this drain's lazy sweep.
    pub expired: u64,
    /// The sequence number the next drain should resume from: one past the
    /// highest returned, or the caller's cursor when nothing was waiting.
    pub next_seq: u64,
    /// Messages still queued for this device after the drain.
    pub queued: u64,
}

/// Per-device inbox counters, as the operator sees them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InboxStats {
    pub queued: u64,
    pub bytes: u64,
    pub dropped_total: u64,
    pub expired_total: u64,
}

/// The inbox: one store, one set of bounds, all of it durable.
pub struct Inbox {
    store: RelayStore,
    limits: InboxLimits,
}

impl Inbox {
    #[must_use]
    pub fn new(store: RelayStore, limits: InboxLimits) -> Self {
        Self { store, limits }
    }

    #[must_use]
    pub fn limits(&self) -> InboxLimits {
        self.limits
    }

    /// Commit one payload for `device` and report what it cost.
    ///
    /// One transaction: sweep what has expired, evict oldest-first until the new
    /// message fits, insert, and update the counters — so the sender is told
    /// `queued` only after the bytes are durable, and a crash cannot leave the
    /// counters disagreeing with the rows.
    pub fn enqueue(
        &self,
        device: &str,
        payload: &[u8],
        now_ms: i64,
    ) -> Result<Enqueued, InboxError> {
        let size = payload.len() as u64;
        if size > self.limits.max_bytes {
            return Err(InboxError::TooLarge {
                device: device.to_string(),
                size,
                bound: self.limits.max_bytes,
            });
        }
        let mut conn = self.store.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let expired = Self::sweep_locked(&tx, now_ms)?;
        if expired > 0 {
            Self::bump_expired(&tx, device, expired)?;
        }

        let (mut queued, mut bytes) = Self::totals_locked(&tx, device)?;
        let mut evicted = Vec::new();
        // Oldest-first, until this message fits. Both bounds are checked before
        // the write, so a full inbox never exceeds its bound.
        while queued + 1 > self.limits.max_messages || bytes + size > self.limits.max_bytes {
            let Some(oldest) = Self::oldest_locked(&tx, device)? else {
                break;
            };
            tx.execute(
                "DELETE FROM inbox WHERE device_id = ?1 AND seq = ?2",
                params![device, oldest.0],
            )?;
            queued -= 1;
            bytes -= oldest.1;
            evicted.push(oldest.0);
        }

        let seq: u64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM inbox WHERE device_id = ?1",
            params![device],
            |row| row.get::<_, i64>(0),
        )? as u64;
        tx.execute(
            "INSERT INTO inbox(device_id, seq, received_at_ms, expires_at_ms, bytes)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                device,
                seq as i64,
                now_ms,
                now_ms + self.limits.ttl_ms,
                payload
            ],
        )?;
        queued += 1;
        bytes += size;

        tx.execute(
            "INSERT INTO inbox_stats(device_id, dropped_total, expired_total, bytes, queued)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(device_id) DO UPDATE SET
               dropped_total = inbox_stats.dropped_total + ?2,
               bytes = ?4,
               queued = ?5",
            params![
                device,
                evicted.len() as i64,
                expired as i64,
                bytes as i64,
                queued as i64
            ],
        )?;
        let dropped_total: i64 = tx.query_row(
            "SELECT dropped_total FROM inbox_stats WHERE device_id = ?1",
            params![device],
            |row| row.get(0),
        )?;
        tx.commit()?;
        // The lock is released *before* the trail is written: `record` takes the
        // same mutex, and holding it here would deadlock rather than merely be
        // slow. Recording inside the transaction was rejected for a second reason
        // — a message's durability must not depend on the audit write succeeding —
        // but this one would have been the bug.
        drop(conn);

        // The trail, after the commit (T-0053). Both facts are recorded by the
        // side that knows them: this is where a drop is discovered, and
        // `note_expiry` is where an expiry is.
        self.note_expiry(expired);
        if !evicted.is_empty() {
            self.record(
                crate::audit::RelayAuditEvent::new(
                    crate::audit::actions::INBOX_DROP,
                    AuditOutcome::Ok,
                )
                .device(device.to_string())
                .detail(format!(
                    "evicted {} oldest message(s): the queue is full",
                    evicted.len()
                )),
            );
        }

        Ok(Enqueued {
            seq,
            queued,
            evicted,
            dropped_total: dropped_total as u64,
        })
    }

    /// Read what is waiting for `device`, from `from_seq` onward.
    ///
    /// The rows are *not* deleted: a consumer that dies mid-drain sees them
    /// again, and only [`Inbox::ack`] removes them. That is what makes the
    /// redelivery path work, and why the consumer must dedupe.
    pub fn drain(
        &self,
        device: &str,
        from_seq: u64,
        limit: usize,
        now_ms: i64,
    ) -> Result<Drained, InboxError> {
        let mut conn = self.store.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let expired = Self::sweep_locked(&tx, now_ms)?;
        if expired > 0 {
            Self::bump_expired(&tx, device, expired)?;
        }

        let mut messages = Vec::new();
        {
            let mut stmt = tx.prepare(
                "SELECT seq, bytes FROM inbox
                 WHERE device_id = ?1 AND seq >= ?2
                 ORDER BY seq ASC LIMIT ?3",
            )?;
            let mut rows = stmt.query(params![device, from_seq as i64, limit as i64])?;
            while let Some(row) = rows.next()? {
                let seq: i64 = row.get(0)?;
                let bytes: Vec<u8> = row.get(1)?;
                messages.push((seq as u64, bytes));
            }
        }
        let raw: Vec<Vec<u8>> = messages.iter().map(|(_, bytes)| bytes.clone()).collect();
        let next_seq = messages.last().map(|(seq, _)| seq + 1).unwrap_or(from_seq);
        let (queued, bytes) = Self::totals_locked(&tx, device)?;
        // `dropped` is what accumulated since the last drain, so it is reported
        // exactly once: the watermark moves to `dropped_total` in the same
        // transaction. `dropped_total` keeps the lifetime count for the operator.
        let (dropped_total, reported): (i64, i64) = tx
            .query_row(
                "SELECT dropped_total, dropped_reported FROM inbox_stats WHERE device_id = ?1",
                params![device],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .unwrap_or((0, 0));
        let dropped = dropped_total.saturating_sub(reported).max(0) as u64;
        tx.execute(
            "INSERT INTO inbox_stats(device_id, dropped_total, expired_total, bytes, queued, dropped_reported)
             VALUES (?1, ?2, 0, ?3, ?4, ?2)
             ON CONFLICT(device_id) DO UPDATE SET
               bytes = ?3, queued = ?4, dropped_reported = ?2",
            params![device, dropped_total, bytes as i64, queued as i64],
        )?;
        tx.commit()?;
        // Released before the trail is written: `record` takes the same mutex
        // (see `enqueue`).
        drop(conn);
        // A drain expires lazily, so the expiry is recorded from here too: the
        // fact is "these messages are gone", and which call noticed is an
        // implementation detail the operator has no use for.
        self.note_expiry(expired);

        Ok(Drained {
            messages,
            raw,
            dropped,
            expired,
            next_seq,
            queued,
        })
    }

    /// Acknowledge everything up to and including `seq`, advancing the cursor in
    /// the same transaction that deletes the rows.
    ///
    /// Returning the number removed lets a caller distinguish "acked the batch"
    /// from "acked something already gone" (a duplicate ack, which is harmless
    /// and must not be an error).
    pub fn ack(&self, device: &str, seq: u64) -> Result<u64, InboxError> {
        let mut conn = self.store.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let removed = tx.execute(
            "DELETE FROM inbox WHERE device_id = ?1 AND seq <= ?2",
            params![device, seq as i64],
        )?;
        let (queued, bytes) = Self::totals_locked(&tx, device)?;
        tx.execute(
            "INSERT INTO inbox_stats(device_id, dropped_total, expired_total, bytes, queued)
             VALUES (?1, 0, 0, ?2, ?3)
             ON CONFLICT(device_id) DO UPDATE SET bytes = ?2, queued = ?3",
            params![device, bytes as i64, queued as i64],
        )?;
        tx.commit()?;
        Ok(removed as u64)
    }

    /// Expire everything past its TTL, across every device. The hourly sweep.
    pub fn sweep(&self, now_ms: i64) -> Result<u64, InboxError> {
        let mut conn = self.store.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let expired = Self::sweep_locked(&tx, now_ms)?;
        tx.commit()?;
        drop(conn);
        self.note_expiry(expired);
        Ok(expired)
    }

    /// Record that `expired` messages reached their time-to-live.
    ///
    /// **One row per sweep with the count, not one per message.** A sweep can
    /// expire thousands; a trail that grows by thousands of near-identical rows an
    /// hour is a trail nobody reads, and the fact the operator wants — how much
    /// mail aged out, and when — is the count. `inbox_stats.expired_total` remains
    /// the per-device counter; this is the record that it happened at all.
    fn note_expiry(&self, expired: u64) {
        if expired == 0 {
            return;
        }
        self.record(
            crate::audit::RelayAuditEvent::new(
                crate::audit::actions::INBOX_EXPIRE,
                AuditOutcome::Expired,
            )
            .detail(format!(
                "expired {expired} message(s) past their time-to-live"
            )),
        );
    }

    /// Append an audit row, and never let the trail break the mailbox.
    ///
    /// Same policy as the router's: a failed write is logged and dropped. A
    /// message that cannot be expired because the audit write failed would linger
    /// past its TTL and then be *delivered* — which is worse than a gap in the
    /// trail, and the log line marks the gap.
    fn record(&self, event: crate::audit::RelayAuditEvent) {
        if let Err(e) = self.store.record(&event) {
            eprintln!(
                "arreo-relay: cannot write the audit trail ({}): {e}",
                event.action
            );
        }
    }

    /// The counters for one device.
    ///
    /// `queued` and `bytes` are **counted from the rows**, not read from the
    /// cached columns: a sweep deletes rows without touching those columns, so a
    /// cached queue depth goes stale the moment anything expires — and a stat
    /// that reports a queue depth the queue does not have is worse than no stat.
    /// The lifetime drop counters come from `inbox_stats`, which is where they
    /// must be durable.
    pub fn stats(&self, device: &str) -> Result<InboxStats, InboxError> {
        let conn = self.store.lock()?;
        let (queued, bytes): (i64, i64) = conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(LENGTH(bytes)), 0) FROM inbox WHERE device_id = ?1",
            params![device],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let counters: Option<(i64, i64)> = conn
            .query_row(
                "SELECT dropped_total, expired_total FROM inbox_stats WHERE device_id = ?1",
                params![device],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (dropped_total, expired_total) = counters.unwrap_or((0, 0));
        Ok(InboxStats {
            queued: queued.max(0) as u64,
            bytes: bytes.max(0) as u64,
            dropped_total: dropped_total.max(0) as u64,
            expired_total: expired_total.max(0) as u64,
        })
    }

    /// Delete what is past its TTL, returning how many rows went.
    ///
    /// Lazy (called inside enqueue and drain) *and* periodic: a device that never
    /// returns must not keep a self-hosted relay's disk full, so the caller
    /// sweeps hourly regardless of traffic — and there is no per-message timer,
    /// because a timer per row is a timer that leaks.
    fn sweep_locked(tx: &rusqlite::Transaction<'_>, now_ms: i64) -> Result<u64, InboxError> {
        // Per-device expiry counts, before the rows go.
        {
            let mut stmt = tx.prepare(
                "SELECT device_id, COUNT(*) FROM inbox WHERE expires_at_ms <= ?1 GROUP BY device_id",
            )?;
            let mut rows = stmt.query(params![now_ms])?;
            let mut per_device = Vec::new();
            while let Some(row) = rows.next()? {
                per_device.push((row.get::<_, String>(0)?, row.get::<_, i64>(1)?));
            }
            for (device, count) in per_device {
                Self::bump_expired(tx, &device, count as u64)?;
            }
        }
        let removed = tx.execute(
            "DELETE FROM inbox WHERE expires_at_ms <= ?1",
            params![now_ms],
        )?;
        Ok(removed as u64)
    }

    fn bump_expired(
        tx: &rusqlite::Transaction<'_>,
        device: &str,
        count: u64,
    ) -> Result<(), InboxError> {
        if count == 0 {
            return Ok(());
        }
        tx.execute(
            "INSERT INTO inbox_stats(device_id, dropped_total, expired_total, bytes, queued)
             VALUES (?1, ?2, ?2, 0, 0)
             ON CONFLICT(device_id) DO UPDATE SET
               dropped_total = inbox_stats.dropped_total + ?2,
               expired_total = inbox_stats.expired_total + ?2",
            params![device, count as i64],
        )?;
        Ok(())
    }

    fn totals_locked(
        tx: &rusqlite::Transaction<'_>,
        device: &str,
    ) -> Result<(u64, u64), InboxError> {
        let (count, bytes): (i64, i64) = tx.query_row(
            "SELECT COUNT(*), COALESCE(SUM(LENGTH(bytes)), 0) FROM inbox WHERE device_id = ?1",
            params![device],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok((count.max(0) as u64, bytes.max(0) as u64))
    }

    fn oldest_locked(
        tx: &rusqlite::Transaction<'_>,
        device: &str,
    ) -> Result<Option<(u64, u64)>, InboxError> {
        let row: Option<(i64, i64)> = tx
            .query_row(
                "SELECT seq, LENGTH(bytes) FROM inbox WHERE device_id = ?1 ORDER BY seq ASC LIMIT 1",
                params![device],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(row.map(|(seq, len)| (seq as u64, len.max(0) as u64)))
    }
}
