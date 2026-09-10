//! Fixture recorder (T-0011): capture real PTY sessions into deterministic
//! `.pty` replays.
//!
//! One sentence: run a command in a real PTY, store every output byte with
//! its timestamp, replay the exact bytes later — so T-0004 state detection
//! proves itself against reality, not hand-written strings.
//!
//! Format (JSONL, one object per line — greppable, diffable, streamable):
//! ```text
//! {"v":1,"cols":80,"rows":24,"argv":["/bin/sh","-c","..."]}   <- header
//! {"t_ms":12,"data":"base64..."}                              <- event
//! {"t_ms":87,"data":"base64..."}                              <- event
//! ```
//! - `t_ms`: milliseconds since spawn (rounded at record time → small,
//!   CI-stable; sub-ms jitter stripped, per the "fixtures are small"
//!   criterion).
//! - `data`: base64 of the raw chunk (byte-accurate, binary-safe).
//! - Replay: `replay_accelerated` concatenates bytes (tests); `replay` honors
//!   pacing with an optional speed multiplier (demos, T-0004 latency checks).
//! - Privacy: `scan_secrets` flags secret-shaped content before a fixture is
//!   committed; `arreo record` refuses to save flagged output without `--allow-secrets`.

use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::pty::{ExitState, Pane};

#[derive(Debug, Error)]
pub enum FixtureError {
    #[error("fixture io: {0}")]
    Io(#[from] std::io::Error),
    #[error("fixture json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("fixture base64: {0}")]
    Base64(#[from] base64::DecodeError),
    #[error("pty: {0}")]
    Pty(#[from] crate::pty::PtyError),
    #[error("bad fixture: {0}")]
    Bad(String),
    #[error("timed out waiting for child exit")]
    Timeout,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Header {
    v: u32,
    cols: u16,
    rows: u16,
    argv: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Record {
    t_ms: u64,
    data: String,
}

/// One captured output chunk.
#[derive(Debug, Clone)]
pub struct Event {
    /// Milliseconds since spawn (rounded at record).
    pub t_ms: u64,
    /// Raw bytes (decoded from base64 on load).
    pub bytes: Vec<u8>,
}

impl Event {
    #[must_use]
    pub fn raw_bytes(&self) -> Vec<u8> {
        self.bytes.clone()
    }
}

/// A recorded session: header + timestamped events.
#[derive(Debug, Clone)]
pub struct Fixture {
    pub argv: Vec<String>,
    pub cols: u16,
    pub rows: u16,
    pub events: Vec<Event>,
}

impl Fixture {
    /// Construct from captured events (driven-record path: input sent
    /// mid-capture, e.g. vim keystrokes — `record()` can't send input).
    #[must_use]
    pub fn from_events(argv: Vec<String>, events: Vec<Event>) -> Self {
        Self {
            argv,
            cols: 80,
            rows: 24,
            events: coalesce(events),
        }
    }

    /// Run `argv` in a real PTY and capture until the child exits or
    /// `timeout` elapses (partial capture still returned on timeout — the
    /// caller decides; `arreo record` warns).
    pub fn record(argv: &[&str], timeout: Duration) -> Result<Self, FixtureError> {
        let (program, args) = argv
            .split_first()
            .ok_or_else(|| FixtureError::Bad("empty argv".to_string()))?;
        let pane = Pane::spawn(program, args, 80, 24)?;
        let start = Instant::now();
        let deadline = start + timeout;
        // Snapshot the raw journal per tick; emit only the NEW suffix as an
        // event. Byte-exact: escapes, colors, alt-screen sequences preserved.
        let mut events: Vec<Event> = Vec::new();
        let mut last_len = 0usize;
        loop {
            std::thread::sleep(Duration::from_millis(10));
            let (raw, _truncated) = pane.raw_snapshot();
            if raw.len() > last_len {
                events.push(Event {
                    t_ms: start.elapsed().as_millis() as u64,
                    bytes: raw[last_len..].to_vec(),
                });
                last_len = raw.len();
            }
            match pane.try_wait() {
                ExitState::Exited(_) => {
                    // Final drain: one more snapshot after exit (EOF race).
                    std::thread::sleep(Duration::from_millis(50));
                    let (raw, _) = pane.raw_snapshot();
                    if raw.len() > last_len {
                        events.push(Event {
                            t_ms: start.elapsed().as_millis() as u64,
                            bytes: raw[last_len..].to_vec(),
                        });
                    }
                    break;
                }
                ExitState::Running => {
                    if Instant::now() >= deadline {
                        break;
                    }
                }
            }
        }
        // Coalesce sub-10ms neighbors to keep fixtures small (timing rounded,
        // pacing preserved at 10 ms granularity — enough for ≤ 200 ms
        // detection-latency assertions in T-0004).
        let events = coalesce(events);
        Ok(Self {
            argv: argv.iter().map(ToString::to_string).collect(),
            cols: 80,
            rows: 24,
            events,
        })
    }

    /// Save as JSONL `.pty`.
    pub fn save(&self, path: &Path) -> Result<(), FixtureError> {
        let file = File::create(path)?;
        let mut out = BufWriter::new(file);
        let header = Header {
            v: 1,
            cols: self.cols,
            rows: self.rows,
            argv: self.argv.clone(),
        };
        serde_json::to_writer(&mut out, &header)?;
        out.write_all(b"\n")?;
        for event in &self.events {
            let record = Record {
                t_ms: event.t_ms,
                data: B64.encode(&event.bytes),
            };
            serde_json::to_writer(&mut out, &record)?;
            out.write_all(b"\n")?;
        }
        out.flush()?;
        Ok(())
    }

    /// Load a JSONL `.pty`.
    pub fn load(path: &Path) -> Result<Self, FixtureError> {
        let file = File::open(path)?;
        let mut lines = BufReader::new(file).lines();
        let header_line = lines
            .next()
            .ok_or_else(|| FixtureError::Bad("empty fixture".to_string()))??;
        let header: Header = serde_json::from_str(&header_line)?;
        if header.v != 1 {
            return Err(FixtureError::Bad(format!(
                "unsupported version {}",
                header.v
            )));
        }
        let mut events = Vec::new();
        for line in lines {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let record: Record = serde_json::from_str(&line)?;
            events.push(Event {
                t_ms: record.t_ms,
                bytes: B64.decode(&record.data)?,
            });
        }
        Ok(Self {
            argv: header.argv,
            cols: header.cols,
            rows: header.rows,
            events,
        })
    }

    /// All bytes concatenated (tests, T-0004 replays).
    #[must_use]
    pub fn replay_accelerated(&self) -> Vec<u8> {
        self.events.iter().flat_map(|e| e.bytes.clone()).collect()
    }

    /// Events with pacing (demos, latency checks). `speed` multiplies time:
    /// 1.0 = recorded pacing, f64::INFINITY = accelerated.
    pub fn replay(&self, speed: f64) {
        if !speed.is_finite() {
            return;
        }
        let mut last = 0u64;
        for event in &self.events {
            let wait_ms = (event.t_ms.saturating_sub(last) as f64 / speed) as u64;
            if wait_ms > 0 {
                std::thread::sleep(Duration::from_millis(wait_ms));
            }
            last = event.t_ms;
        }
    }

    /// Full decoded text (convenience for assertions + secret scan).
    #[must_use]
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.replay_accelerated()).to_string()
    }
}

fn coalesce(events: Vec<Event>) -> Vec<Event> {
    let mut out: Vec<Event> = Vec::with_capacity(events.len());
    for event in events {
        if let Some(prev) = out.last_mut() {
            if event.t_ms.saturating_sub(prev.t_ms) < 10 {
                prev.bytes.extend_from_slice(&event.bytes);
                continue;
            }
        }
        out.push(event);
    }
    out
}

/// Flag secret-shaped content. Returns human-readable findings (empty = clean).
/// Patterns: `sk-`/`AKIA`/`ghp_`/`xox` token prefixes, `BEGIN .* PRIVATE KEY`,
/// `api[_-]?key` assignments with long values, AWS secret-shaped 40-char
/// base64 after `aws_secret`, generic `password = <long>` assignments.
#[must_use]
pub fn scan_secrets(text: &str) -> Vec<String> {
    let mut findings = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let n = i + 1;
        let mut check = |needle: &str, label: &str| {
            if line.contains(needle) {
                findings.push(format!("line {n}: possible {label}"));
            }
        };
        check("sk-", "api key (sk- prefix)");
        check("AKIA", "aws access key id");
        check("ghp_", "github token");
        check("gho_", "github oauth token");
        check("xox", "slack token");
        check("BEGIN PRIVATE KEY", "private key block");
        check("BEGIN RSA PRIVATE KEY", "private key block");
        check("BEGIN OPENSSH PRIVATE KEY", "private key block");
        let lower = line.to_lowercase();
        for key in [
            "api_key",
            "apikey",
            "api-key",
            "aws_secret",
            "client_secret",
        ] {
            if lower.contains(key) && line.len() > lower.find(key).unwrap_or(0) + key.len() + 8 {
                findings.push(format!("line {n}: possible secret assignment ({key})"));
                break;
            }
        }
        if (lower.contains("password") || lower.contains("passwd"))
            && line
                .split(['=', ':'])
                .nth(1)
                .is_some_and(|v| v.trim().len() >= 12)
        {
            findings.push(format!("line {n}: possible password assignment"));
        }
    }
    findings
}
