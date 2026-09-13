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
                    // Final drain: poll until the journal is STABLE (two
                    // equal snapshots 100 ms apart, up to 2 s) — children
                    // often flush on exit and the reader pump needs
                    // scheduling on a loaded box (chaos-found: fixed 50 ms
                    // missed pi's exit flush 4 runs straight).
                    let mut stable = 0u32;
                    let mut last = last_len;
                    for _ in 0..20 {
                        std::thread::sleep(Duration::from_millis(100));
                        let (raw, _) = pane.raw_snapshot();
                        if raw.len() > last {
                            events.push(Event {
                                t_ms: start.elapsed().as_millis() as u64,
                                bytes: raw[last..].to_vec(),
                            });
                            last = raw.len();
                            stable = 0;
                        } else {
                            stable += 1;
                            if stable >= 2 {
                                break;
                            }
                        }
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

/// The prefixes whose tokens are secret-shaped, the label the scan reports, and
/// the shortest run after the prefix that is worth calling a token.
///
/// Shared by the scanner and the masker on purpose: when the two disagreed about
/// what a token is, the scanner flagged a line the masker left alone — a secret
/// on disk beside `redacted = 1`. One definition, two callers.
///
/// The run length is per prefix rather than global because the prefixes differ
/// in how much entropy follows them: `sk-` is distinctive enough that eight
/// characters mean something, while `AIza` (Google) and `eyJ` (a JWT's base64url
/// header) are ordinary letter runs — `AIza is the start of a key` is prose, and
/// a three-letter `eyJ` word is not a token. The shapes and their lengths come
/// from the harness survey's measured probes (T-0075/T-0080); the older five are
/// unchanged so the fixtures and audit rows they were tuned against stay put.
pub const TOKEN_PREFIXES: [(&str, &str, usize); 11] = [
    ("sk-", "api key (sk- prefix)", 8),
    ("AKIA", "aws access key id", 16),
    ("ghp_", "github token", 8),
    ("gho_", "github oauth token", 8),
    ("xox", "slack token", 8),
    ("vbk_", "verboo api key", 8),
    ("xai-", "xai api key", 8),
    ("glpat-", "gitlab token", 8),
    ("hf_", "huggingface token", 8),
    // A Google API key is exactly `AIza` + 35; 30 keeps a near-miss out while
    // catching every real one.
    ("AIza", "google api key", 30),
    // A JWT is three dot-separated base64url runs; the run scanner does not stop
    // at `.`, so the whole token is one run. 40 is well inside the shortest real
    // token and well outside the word "eyJ" in a sentence.
    ("eyJ", "jwt", 40),
];
/// The floor any prefix's run must clear: below it nothing is a token, whatever
/// the prefix. Kept public because it is the number the docs and the tests quote
/// when they say "a short run is not a token".
pub const MIN_TOKEN_RUN: usize = 8;

/// The earliest secret-shaped token in `line`: where it starts, how long it is
/// (prefix included), and the label the scan reports it under.
///
/// A token runs from its prefix to the first character that cannot be part of
/// one — whitespace or punctuation — so a quoted or bracketed token is still a
/// token. A prefix followed by too short a run is not one: `ask-me` contains
/// `sk-`, and flagging it would have masked a word nobody would call a secret.
pub fn find_token(line: &str) -> Option<(usize, usize, &'static str)> {
    let mut best: Option<(usize, usize, &'static str)> = None;
    for (prefix, label, min_run) in TOKEN_PREFIXES {
        let mut from = 0;
        while let Some(rel) = line[from..].find(prefix) {
            let at = from + rel;
            let run = token_run_len(&line[at + prefix.len()..]);
            if prefix.len() + run >= min_run.max(MIN_TOKEN_RUN) {
                if best.is_none_or(|(seen, _, _)| at < seen) {
                    best = Some((at, prefix.len() + run, label));
                }
                break;
            }
            from = at + prefix.len();
        }
    }
    best
}

/// How long the token run starting at `rest` is: up to the first character that
/// cannot appear in a token.
fn token_run_len(rest: &str) -> usize {
    rest.find(|c: char| {
        c.is_whitespace() || matches!(c, '"' | '\'' | '`' | ',' | ';' | ')' | ']' | '}' | '>')
    })
    .unwrap_or(rest.len())
}

/// Is `value` a **reference** to a secret this machine resolves itself, rather
/// than the secret? (T-0083.)
///
/// §3.8's rule is that a synced file carries the *name* and the machine supplies
/// the value, so the scan has to tell the two apart or it refuses exactly the
/// configuration the operator was told to write. The dialects are the ones the
/// harness survey measured, not a guess: opencode `{env:NAME}`, pi `$NAME` and
/// `${NAME}`, omp the bare variable name (measured twice — the sigil forms are
/// sent literally as the bearer token and 401, T-0080).
///
/// ## The residual, stated rather than hidden
///
/// A literal that happens to be shaped like a variable name — all uppercase, no
/// prefix any provider uses — is indistinguishable from a reference *here*, and
/// this function calls it one. That is deliberate: the alternative (flagging
/// bare names) refuses omp's only working dialect, which is the false positive
/// this function exists to remove. What closes the gap is not more cleverness in
/// a text scan but the sync path (T-0083): a file whose reference does not
/// resolve to a variable on the receiving machine is refused by name. Every
/// provider-prefixed literal is still caught by [`find_token`], which reads the
/// value's shape and never consults this function.
#[must_use]
pub fn is_env_reference(value: &str) -> bool {
    let value = value
        .trim()
        .trim_matches(|c| c == '"' || c == '\'' || c == ',' || c == ';');
    if value.is_empty() {
        return false;
    }
    for (open, close) in [("{env:", '}'), ("${", '}')] {
        if let Some(rest) = value.strip_prefix(open) {
            return rest.strip_suffix(close).is_some_and(is_env_name);
        }
    }
    if let Some(rest) = value.strip_prefix('$') {
        return is_env_name(rest);
    }
    // omp's dialect: the bare name. See the residual note above.
    is_env_name(value)
}

/// Does `name` look like an environment variable rather than a secret? Uppercase
/// letters, digits and underscores, starting with a letter, with at least one
/// letter — the shape every shell, `.env` and container runtime uses.
fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_uppercase() {
        return false;
    }
    if name.len() > 64 {
        return false;
    }
    name.chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// The value side of `line`, if it has an assignment separator: everything after
/// the first `=` or `:`, trimmed of the quotes and whitespace a config file wraps
/// it in.
fn assignment_value(line: &str) -> Option<&str> {
    let at = line.find(['=', ':'])?;
    Some(line[at + 1..].trim().trim_matches(['"', '\'']))
}

/// Flag secret-shaped content. Returns human-readable findings (empty = clean).
/// Patterns: the token prefixes in [`TOKEN_PREFIXES`], `BEGIN .* PRIVATE KEY`,
/// `api[_-]?key` assignments with long values, AWS secret-shaped 40-char
/// base64 after `aws_secret`, generic `password = <long>` assignments — and,
/// since T-0083, **not** an assignment whose value is an env reference
/// ([`is_env_reference`]): §3.8 tells the operator to write one, so refusing it
/// would refuse the correct configuration.
///
/// A token is defined by [`find_token`], the same function the
/// masker uses: a prefix followed by a long enough run. The scanner and the
/// masker must agree, or a flagged line goes to disk unmasked.
#[must_use]
pub fn scan_secrets(text: &str) -> Vec<String> {
    let mut findings = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let n = i + 1;
        let mut tokens = 0;
        let mut rest = line;
        while let Some((at, len, label)) = find_token(rest) {
            // One finding per token, not one per line: a line with two secrets
            // is two things an operator wants to see.
            findings.push(format!("line {n}: possible {label}"));
            rest = &rest[at + len..];
            tokens += 1;
            if tokens > 8 {
                break;
            }
        }
        for needle in [
            "BEGIN PRIVATE KEY",
            "BEGIN RSA PRIVATE KEY",
            "BEGIN OPENSSH PRIVATE KEY",
        ] {
            if line.contains(needle) {
                findings.push(format!("line {n}: possible private key block"));
            }
        }
        let lower = line.to_lowercase();
        // A reference in the value is the point of §3.8, not a finding: the file
        // names the variable and the machine holds the secret. Read the value,
        // never the field name alone — that was the bug.
        let reference = assignment_value(line).is_some_and(is_env_reference);
        if !reference {
            for key in [
                "api_key",
                "apikey",
                "api-key",
                "aws_secret",
                "client_secret",
            ] {
                if lower.contains(key) && line.len() > lower.find(key).unwrap_or(0) + key.len() + 8
                {
                    findings.push(format!("line {n}: possible secret assignment ({key})"));
                    break;
                }
            }
        }
        if !reference
            && (lower.contains("password") || lower.contains("passwd"))
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
