//! Daemon persistence (T-0018): snapshot the registry to SQLite, restore on boot.
//!
//! One sentence: every spawn/exit/drain-tick snapshots pane specs +
//! scrollback lines; every boot respawns recorded commands and replays
//! scrollback bytes into the fresh panes, so `kill -9` + restart restores
//! layout and history byte-identical (modulo live-child differences: the new
//! child re-runs the recorded command, so *history* is restored, not the
//! exact pre-crash process).
//!
//! Strategy (respawn+replay, NOT fd-passing): fd-passing cannot survive a
//! machine reboot (the stated criterion); respawn+replay survives both crash
//! and reboot with one mechanism. Tradeoff, stated openly: the restored pane
//! runs a NEW child (new PID, command re-executed) — scrollback shows the old
//! history followed by the fresh run's output. That is Herdr-grade restore.
//!
//! DB path: alongside the socket (`<socket>.db`, e.g. `/tmp/arreo-x.sock` →
//! `/tmp/arreo-x.sock.db`). No config needed, no second path to lose.

use arreo_core::pty::Pane;
use arreo_core::store::{SessionStore, StoredPane};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PersistError {
    #[error("persist store: {0}")]
    Store(#[from] arreo_core::store::SessionError),
    #[error("persist pty: {0}")]
    Pty(String),
}

/// DB path for a socket path (`<socket>.db`).
#[must_use]
pub fn db_path_for(socket: &Path) -> PathBuf {
    // One rule, shared with the device authority and the CLI (T-0025).
    arreo_core::identity::authority::sidecar_db(socket)
}

/// Snapshot `panes` (id → pane) into the DB at `socket`'s sidecar path.
/// Returns panes saved. Scrollback comes from `drain()` (decoded lines —
/// both save and restore go through `drain`, so equality is exact).
pub fn snapshot(panes: &[(String, Arc<Pane>)], db: &Path) -> Result<usize, PersistError> {
    let store = match SessionStore::open(db) {
        Ok(store) => store,
        Err(e) => {
            // Heal once: a corrupt DB (disk rot, partial write) is moved
            // aside with a timestamp (evidence preserved) and recreated.
            // Without this, every op would fail-log forever after one bad byte.
            let backup = db.with_extension(format!(
                "corrupt-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
            ));
            eprintln!(
                "persist: {e} — moving aside to {} and recreating",
                backup.display()
            );
            let _ = std::fs::rename(db, &backup);
            SessionStore::open(db)?
        }
    };
    let records: Vec<StoredPane> = panes
        .iter()
        .map(|(id, pane)| {
            let spec = pane.spawn_spec();
            let (cols, rows) = pane.size().unwrap_or((80, 24));
            StoredPane {
                id: id.clone(),
                program: spec.program,
                args: spec.args,
                cols,
                rows,
                scrollback: pane.drain(),
            }
        })
        .collect();
    let count = records.len();
    store.save_topology(&records)?;
    Ok(count)
}

/// Restore: respawn every recorded pane and replay its scrollback lines as
/// input bytes (so the fresh pane's journal + drain show the old history
/// first). Returns (id, pane) pairs ready to insert into a registry.
/// Panes whose command fails to respawn are SKIPPED loudly (eprintln) — one
/// bad record never blocks the rest of the restore.
pub fn restore(db: &Path) -> Result<Vec<(String, Arc<Pane>)>, PersistError> {
    if !db.exists() {
        return Ok(Vec::new());
    }
    let store = SessionStore::open(db)?;
    let records = store.load_topology()?;
    let mut out = Vec::new();
    for record in records {
        let args: Vec<&str> = record.args.iter().map(String::as_str).collect();
        let pane = match Pane::spawn(&record.program, &args, record.cols, record.rows) {
            Ok(pane) => Arc::new(pane),
            Err(e) => {
                eprintln!("persist: skipping {} (respawn failed: {e})", record.id);
                continue;
            }
        };
        // Replay scrollback SAFELY: pre-seed the ring via restore_history
        // (never as keystrokes — typing history would EXECUTE metachar lines
        // in the fresh shell; proven hazard, T-0018 adversarial pass).
        pane.restore_history(&record.scrollback);
        out.push((record.id, pane));
    }
    Ok(out)
}
