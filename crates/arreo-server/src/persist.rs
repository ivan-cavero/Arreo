//! Daemon persistence (T-0018 + T-0072): snapshot the registry to SQLite,
//! restore on boot.
//!
//! One sentence: every spawn/exit/drain-tick snapshots pane specs + scrollback
//! lines and — since T-0072 — the harness session each pane is on; every boot
//! respawns recorded commands, resumes the harness session where the adapter
//! declares a strategy for it, and replays scrollback bytes into the fresh
//! panes, so `kill -9` + restart restores layout and history byte-identical
//! (modulo live-child differences: the new child re-runs the recorded
//! command, so *history* is restored, while a resumed harness session
//! continues where the old child left off).
//!
//! Strategy (respawn+replay, NOT fd-passing): fd-passing cannot survive a
//! machine reboot (the stated criterion); respawn+replay survives both crash
//! and reboot with one mechanism. Since T-0072 the resume argv is built from
//! the record's `harness` + `session_id` through the adapter registry's data —
//! never from keystrokes (T-0018's proven hazard stays fixed), and never
//! hardcoded per harness. A pane with no strategy, or one whose resume fails,
//! falls back loudly to today's respawn+history.
//!
//! DB path: alongside the socket (`<socket>.db`, e.g. `/tmp/arreo-x.sock` →
//! `/tmp/arreo-x.sock.db`). No config needed, no second path to lose.

use arreo_core::pty::{ExitState, Pane};
use arreo_core::state::AdapterRegistry;
use arreo_core::store::{SessionStore, StoredPane};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
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

/// `<socket>.lock` — the single-instance lock a serving daemon holds (T-0071).
pub fn lock_path_for(socket: &Path) -> PathBuf {
    arreo_core::identity::authority::sidecar(socket, ".lock")
}

/// One pane's live state as the daemon knows it at snapshot time: the pane
/// itself plus the record columns only the daemon knows (which harness it was
/// started under, and the session it is on).
pub struct SnapshotPane {
    pub id: String,
    pub pane: Arc<Pane>,
    /// The adapter registry id the pane's program matched at spawn (T-0072),
    /// `None` for a pane no adapter claims.
    pub harness: Option<String>,
    /// The harness session this pane is on: the id Arreo pinned at spawn, or
    /// the one captured from the harness's own output once it printed any
    /// (read from the entry's engine when it snapshots).
    pub session_id: Option<String>,
}

/// Snapshot `panes` (id → pane + harness/session) into the DB at `socket`'s
/// sidecar path. Returns panes saved. Scrollback comes from `drain()` (decoded
/// lines — both save and restore go through `drain`, so equality is exact).
pub fn snapshot(panes: &[SnapshotPane], db: &Path) -> Result<usize, PersistError> {
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
        .map(|p| {
            let spec = p.pane.spawn_spec();
            let (cols, rows) = p.pane.size().unwrap_or((80, 24));
            StoredPane {
                id: p.id.clone(),
                program: spec.program,
                args: spec.args,
                cols,
                rows,
                scrollback: p.pane.drain(),
                harness: p.harness.clone(),
                session_id: p.session_id.clone(),
            }
        })
        .collect();
    let count = records.len();
    store.save_topology(&records)?;
    Ok(count)
}

/// A pane restored for the registry (T-0072): the fresh child plus the record
/// facts the daemon re-hangs on it — which harness it runs under and which
/// session it resumed — so the *next* snapshot keeps them (a restored pane
/// must not forget, on its very first write, the very thing it was restored
/// with).
pub struct RestoredPane {
    pub id: String,
    pub pane: Arc<Pane>,
    pub harness: Option<String>,
    pub session_id: Option<String>,
}

/// The grace a resumed child gets to refuse the resume before the restore
/// treats it as a refusal: a harness told `--session <unknown>` dies in a few
/// hundred ms with a non-zero status (opencode 1.18.30 does), while a healthy
/// resumed run lives for the length of a model turn. Long enough to catch a
/// refusal, short enough that a boot with several resumed panes is not
/// serialized on sleeps.
const RESUME_REFUSAL_GRACE: Duration = Duration::from_millis(400);

/// Restore: for every recorded pane, spawn it — with the resume argv its
/// adapter strategy builds from `harness` + `session_id` when the record has
/// one, else exactly as before (respawn the command) — replay its scrollback
/// lines as history (never as keystrokes; T-0018's proven hazard stays fixed),
/// and return the panes ready to insert into a registry.
///
/// **One bad record never blocks the rest.** A pane whose command cannot
/// respawn is SKIPPED loudly; a pane whose resume the harness refuses (the
/// child dies within [`RESUME_REFUSAL_GRACE`] with a non-zero status) falls
/// back loudly to the plain respawn — and so does a pane whose record names a
/// harness the registry no longer knows, or a `pin` strategy without an id.
/// What the messages never carry is the session id itself: an id identifies a
/// session; an operator who needs one reads it from the harness, not from
/// daemon stderr.
pub fn restore(db: &Path, adapters: &AdapterRegistry) -> Result<Vec<RestoredPane>, PersistError> {
    if !db.exists() {
        return Ok(Vec::new());
    }
    let store = SessionStore::open(db)?;
    let records = store.load_topology()?;
    let mut out = Vec::new();
    for record in records {
        // The adapter the record names (or its program implies); `None` plan
        // means "no resume possible from this record" — the plain path.
        let adapter = adapters.for_record(record.harness.as_deref(), &record.program);
        let plan = adapter.resume.as_ref().and_then(|resume| {
            // The record's args may already carry the resume argv (a `pin`
            // pane was spawned with it on purpose): strip it back to base so
            // `resume_args` re-applies it, never stacks a second flag.
            let (base, carried) = resume.base_args(&record.args);
            let session = record.session_id.as_deref().or(carried.as_deref());
            resume
                .resume_args(&base, session)
                .map(|args| (args, session.map(str::to_string)))
        });

        // Spawn on the resume argv when there is one, else plainly. A resume
        // the harness refuses (the child dies within the grace, non-zero) and
        // a resume spawn that fails both fall back to the plain respawn —
        // loudly. `loud` collects the "this record did not resume" lines, so
        // the fallback that still brings a pane back prints as loudly as the
        // skip that cannot.
        let mut loud: Vec<String> = Vec::new();
        let spawned: Result<(Arc<Pane>, Option<String>), String> = match plan {
            None => {
                // The no-resume branch has two shapes, and only one may be
                // silent: a record that never named a harness takes the plain
                // path because there is nothing to resume, and an operator who
                // spawned `/bin/sh` knows its first frame is fresh. A record
                // that *did* name a harness yet yields no resume argv — a
                // harness the registry no longer knows, or a `pin` record
                // whose id did not survive — must say so: without the line,
                // the restored pane looks resumed and is a fresh child, which
                // is exactly the failure the loud contract exists to prevent.
                if let Some(reason) =
                    no_resume_reason(record.harness.as_deref(), record.session_id.as_deref())
                {
                    loud.push(reason);
                }
                spawn_plain(&record).map(|pane| (pane, None))
            }
            Some((args, session)) => {
                let args_ref: Vec<&str> = args.iter().map(String::as_str).collect();
                match Pane::spawn(&record.program, &args_ref, record.cols, record.rows) {
                    Ok(pane) => {
                        let pane = Arc::new(pane);
                        // A run that survives the grace (or exits cleanly) is
                        // a resume that worked; a non-zero death inside the
                        // grace is a refusal (opencode's `--session <unknown>`
                        // dies just so). Assuming a resume over a dead child
                        // would claim a continued session that is not there.
                        match pane.wait_timeout(RESUME_REFUSAL_GRACE) {
                            // The only failure shape: a non-zero death inside
                            // the grace (a refusal). Still running, still
                            // Running, or a clean exit is a resume that
                            // worked.
                            Some(ExitState::Exited(code)) if code != 0 => {
                                loud.push(format!(
                                    "harness refused the resume (exit {code}); respawning plainly"
                                ));
                                drop(pane); // already reaped
                                spawn_plain(&record).map(|pane| (pane, None))
                            }
                            _ => Ok((pane, session)),
                        }
                    }
                    Err(e) => {
                        loud.push(format!("resume spawn failed: {e}; respawning plainly"));
                        spawn_plain(&record).map(|pane| (pane, None))
                    }
                }
            }
        };

        match spawned {
            Ok((pane, session)) => {
                // History is engine input, never child keystrokes — typing
                // recorded lines would EXECUTE metachar text in the fresh
                // shell (proven hazard, T-0018 adversarial pass).
                pane.restore_history(&record.scrollback);
                if !loud.is_empty() {
                    skip_loudly(&record, adapter.harness_id(), &loud.join("; "));
                }
                out.push(RestoredPane {
                    id: record.id,
                    pane,
                    harness: adapter.harness_id().map(str::to_string),
                    session_id: session,
                });
            }
            Err(why) => {
                if loud.is_empty() {
                    loud.push("respawn failed".to_string());
                }
                skip_loudly(
                    &record,
                    adapter.harness_id(),
                    &format!("{} — {why}", loud.join("; ")),
                );
            }
        }
    }
    Ok(out)
}

fn spawn_plain(record: &StoredPane) -> Result<Arc<Pane>, String> {
    let args: Vec<&str> = record.args.iter().map(String::as_str).collect();
    Pane::spawn(&record.program, &args, record.cols, record.rows)
        .map(Arc::new)
        .map_err(|e| format!("respawn failed: {e}"))
}

/// The one line a fallen-back (or skipped) restore prints: the pane id, the
/// harness that answered for it, and why. **The session id is not in it and
/// never can be** (T-0072 safety): the record is passed whole so the rule is
/// not a convention at the call sites, a session id identifies a session, and
/// daemon stderr is broadcast-shaped by comparison — an operator who needs an
/// id reads it from the harness, which prints it to its own pane.
/// Why a record that names a harness still has no resume argv, as the loud
/// notice would state it — or `None` when the record never claimed a harness
/// (the plain path is then by design, not by failure, and stays silent).
///
/// Two shapes: the registry no longer knows the harness (a record from a
/// removed adapter, or one whose `harness` id was mistyped), and a harness
/// whose `pin` strategy records an id that did not survive (the record names
/// the harness but carries none). Both mean "this pane is fresh, not resumed",
/// which an operator must be able to distinguish from a holdover session.
pub fn no_resume_reason(harness: Option<&str>, session_id: Option<&str>) -> Option<String> {
    harness.map(|harness| {
        let why = match session_id {
            Some(_) => format!("the registry has no resume strategy for harness {harness:?}"),
            None => "the record names a harness but carries no session id".to_string(),
        };
        format!("no resume possible: {why}; respawning plainly")
    })
}

fn fallback_notice(record: &StoredPane, harness: Option<&str>, message: &str) -> String {
    format!(
        "persist: pane {:?} ({}): {message}",
        record.id,
        harness.unwrap_or("none")
    )
}

fn skip_loudly(record: &StoredPane, harness: Option<&str>, message: &str) {
    eprintln!("{}", fallback_notice(record, harness, message));
}

#[cfg(test)]
mod tests {
    //! The one-invariant test that a transcript cannot check: the loud notice
    //! a failed resume prints carries the pane's identity and the reason, and
    //! never the session id (T-0072 safety criterion).

    use super::*;

    fn record_with_session() -> StoredPane {
        StoredPane {
            id: "pane-a".to_string(),
            program: "pi".to_string(),
            args: vec!["--session-id".to_string(), SESSION.to_string()],
            cols: 80,
            rows: 24,
            scrollback: Vec::new(),
            harness: Some("pi".to_string()),
            session_id: Some(SESSION.to_string()),
        }
    }

    const SESSION: &str = "01a099cd-2d35-74c4-90c6-d0e3b10011b5";

    #[test]
    fn the_fallback_notice_never_carries_a_session_id() {
        let record = record_with_session();
        let line = fallback_notice(
            &record,
            Some("pi"),
            "harness refused the resume (exit 1); respawning plainly",
        );
        assert!(
            !line.contains(SESSION),
            "a restore notice must not expose the session id: {line}"
        );
        assert!(
            line.contains("pane-a") && line.contains("(pi)") && line.contains("refused"),
            "the line stays useful without the id: {line}"
        );
    }
}
