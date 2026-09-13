//! Detection engine: explicit-clock bytes → state events.

use super::adapter::Adapter;

/// Pane state (ROADMAP §3.9). `Question` is always `Inferred` at the
/// universal tier — the label is honesty, not hedging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Unknown,
    Working,
    Idle,
    Question,
    Blocked,
    Done,
}

/// How much to trust the event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confidence {
    /// Direct signal (output flowing, child exited, bell).
    Direct,
    /// Pattern/silence inference — carries the rule that fired.
    Inferred { rule: String },
}

/// One state transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// Engine-clock timestamp (== the feed's `now_ms` for output-driven
    /// transitions → latency budget holds by construction).
    pub t_ms: u64,
    pub state: State,
    pub confidence: Confidence,
    /// The adapter pattern that fired (question/error), if any.
    pub matched_pattern: Option<String>,
    /// Exit code for `Done`, else `None`.
    pub exit_code: Option<i32>,
}

fn strip_ansi(bytes: &[u8]) -> String {
    // Minimal SGR/CSI stripper for tail matching: drop ESC [ ... final-byte
    // sequences and C0 controls except \n \t. The grid (T-0003) already
    // decodes cells; this keeps the engine usable on raw streams.
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c2 in chars.by_ref() {
                    if c2.is_ascii_alphabetic() || c2 == '~' {
                        break;
                    }
                }
            }
            continue;
        }
        if c.is_control() && c != '\n' && c != '\t' {
            continue;
        }
        out.push(c);
    }
    out
}

/// Scan `bytes` for a bare BEL (one that is not an OSC terminator),
/// maintaining the escape-stream state across feeds.
///
/// An OSC string (window title, OSC 8 hyperlink, colour query/set) is opened
/// by `ESC ]` and runs until a terminator: BEL (`0x07`, the common one), ST
/// (`ESC \` or C1 `0x9c`), or is aborted by CAN/SUB (`0x18`/`0x1a`). A BEL
/// that terminates (or aborts) nothing — i.e. is seen outside any OSC string
/// — is a real ring-the-bell byte and returns `true`.
///
/// The state lives on the engine, not the caller: the feed path guarantees no
/// alignment, so an OSC string or a two-byte escape can straddle two `feed`
/// calls. `esc_pending` carries an `ESC` seen at the very end of the previous
/// feed; this feed's first byte decides what it opened — `]` starts an OSC
/// string, `\` is the ST half (a no-op when nothing is open), anything else
/// is a two-byte escape that changes nothing here.
///
/// The narrowing is strict by construction: every `0x07` outside an OSC
/// string behaves exactly as before, and no `0x07` inside one ever means
/// attention.
fn scan_bell(bytes: &[u8], osc_active: &mut bool, esc_pending: &mut bool) -> bool {
    let mut bell = false;
    for &b in bytes {
        if *esc_pending {
            // The previous byte was an ESC; this byte completes the pair.
            *esc_pending = false;
            if *osc_active {
                // Inside an OSC string an ESC matters only as the ST half —
                // and a terminator byte still terminates the string wherever
                // it lands (a terminal applies BEL from any position).
                match b {
                    b'\\' | 0x07 | 0x9c | 0x18 | 0x1a => *osc_active = false,
                    _ => {}
                }
            } else {
                match b {
                    // `ESC ]` opens an OSC string.
                    b']' => *osc_active = true,
                    // `ESC BEL` outside any OSC: the BEL is a real bell
                    // (strict narrowing — only OSC content changes meaning).
                    0x07 => bell = true,
                    _ => {}
                }
            }
            continue;
        }
        if *osc_active {
            match b {
                // BEL and C1 ST terminate the OSC string — not a bell.
                0x07 | 0x9c => *osc_active = false,
                // CAN/SUB abort the string (ECMA-48 §5.5.2): the string is
                // over, so a following BEL is a real one again.
                0x18 | 0x1a => *osc_active = false,
                // A stray ESC may start an ST; the next byte decides.
                0x1b => *esc_pending = true,
                _ => {}
            }
        } else {
            match b {
                // Any ESC outside a control string: consume the pair.
                0x1b => *esc_pending = true,
                0x07 => bell = true,
                _ => {}
            }
        }
    }
    bell
}

/// Last N lines of text (tail matching window).
fn tail_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let from = lines.len().saturating_sub(n);
    lines[from..].join("\n")
}

/// Explicit-clock detection engine. All time comes from the caller.
pub struct Engine {
    adapter: Adapter,
    state: State,
    /// Visible text accumulator (capped — see TEXT_CAP).
    text: String,
    /// Engine-clock of the last output byte.
    last_output_ms: Option<u64>,
    /// Whether the last output contained a bare BEL (one that terminated no
    /// OSC string).
    bell_pending: bool,
    /// Whether the output stream is inside an OSC string (opened by `ESC ]`).
    /// A BEL/ST inside one terminates it rather than ringing a bell; the
    /// state persists across feeds because an OSC string can straddle two
    /// feed calls.
    osc_active: bool,
    /// A trailing `ESC` at the end of the previous feed, waiting for the
    /// byte that decides what it opened (`]` → OSC, `\` → ST, anything else
    /// → a two-byte escape). Persisted so a feed boundary cannot split an
    /// escape sequence (T-0080).
    esc_pending: bool,
    /// Whether an error shape was seen since the last state change.
    error_armed: bool,
    exited: bool,
    /// The harness session this pane is on (T-0072): pinned at spawn (the
    /// `pin` strategy) or learned from output (the strategy's
    /// `session_pattern`, when the harness happens to print one). `None` means
    /// "no id known yet" — normal for any pane whose harness prints none.
    session: Option<String>,
    /// Bounded carry of text still in flight for capture (a session id line
    /// can straddle two feeds; the carry carries it across).
    capture_carry: String,
    /// Set exactly when an id was learned from output (not when one was
    /// pinned). The daemon snapshots on this so the record can name a session
    /// that only output revealed.
    session_learned: bool,
}

/// Cap on retained visible text (64 KiB — tail matching only needs the end).
const TEXT_CAP: usize = 64 * 1024;

/// Cap on the capture carry (T-0072): the session id line is printed near
/// spawn, so a few KiB of in-flight text is all a straddle can need; a long
/// matchless run must not grow the carry forever.
const CAPTURE_CARRY_BYTES: usize = 4096;

impl Engine {
    #[must_use]
    pub fn new(adapter: Adapter, _now_ms: u64) -> Self {
        Self {
            adapter,
            state: State::Unknown,
            text: String::new(),
            last_output_ms: None,
            bell_pending: false,
            osc_active: false,
            esc_pending: false,
            error_armed: false,
            exited: false,
            session: None,
            capture_carry: String::new(),
            session_learned: false,
        }
    }

    #[must_use]
    pub fn state(&self) -> &State {
        &self.state
    }

    /// The harness session this pane is on, if one is known (pinned at spawn
    /// or captured from output). `None` is not an error: some harnesses never
    /// print their session.
    #[must_use]
    pub fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }

    /// Record the id this pane was spawned with (the `pin` strategy). A later
    /// output capture may not overwrite it — the id Arreo chose at spawn is
    /// the truth.
    pub fn set_session(&mut self, id: String) {
        if self.session.is_none() {
            self.session = Some(id);
        }
    }

    /// Whether the engine learned a session id from output since the last
    /// call. Set exactly on output capture (never on a pin), so the daemon can
    /// snapshot when a record gains an id that only output revealed.
    pub fn take_session_learned(&mut self) -> bool {
        std::mem::take(&mut self.session_learned)
    }

    /// Scan fresh visible text for this adapter's session id. The carry joins
    /// the previous window, so a session line split across two feeds is still
    /// seen whole; once an id is found (or the pane has no pattern) this never
    /// scans again.
    fn capture(&mut self, fresh: &str) {
        if self.session.is_some() || !self.adapter.captures_session() {
            return;
        }
        self.capture_carry.push_str(fresh);
        let found = self
            .adapter
            .resume
            .as_ref()
            .and_then(|resume| resume.capture(&self.capture_carry));
        if let Some(id) = found {
            self.session = Some(id);
            self.session_learned = true;
            self.capture_carry.clear();
            return;
        }
        if self.capture_carry.len() > CAPTURE_CARRY_BYTES {
            let mut drop = self.capture_carry.len() - CAPTURE_CARRY_BYTES;
            while drop > 0 && !self.capture_carry.is_char_boundary(drop) {
                drop -= 1;
            }
            self.capture_carry.drain(..drop);
        }
    }

    /// Feed raw output bytes observed at `now_ms`. Emits transitions.
    pub fn feed(&mut self, bytes: &[u8], now_ms: u64) -> Vec<Event> {
        let mut events = Vec::new();
        if self.exited {
            return events;
        }
        if !bytes.is_empty() {
            // Escape-aware (T-0080): a BEL that terminates an OSC string is
            // the string's terminator, not a bell; a bare BEL still is.
            if scan_bell(bytes, &mut self.osc_active, &mut self.esc_pending) {
                self.bell_pending = true;
            }
            let visible = strip_ansi(bytes);
            self.text.push_str(&visible);
            if self.text.len() > TEXT_CAP {
                // Floor to a char boundary: byte truncation can split a
                // multibyte char (chaos-found panic: is_char_boundary).
                let mut drop = self.text.len() - TEXT_CAP;
                while drop > 0 && !self.text.is_char_boundary(drop) {
                    drop -= 1;
                }
                self.text.drain(..drop);
            }
            if self.adapter.match_error(&visible) {
                self.error_armed = true;
            }
            self.capture(&visible);
            self.last_output_ms = Some(now_ms);
            // Output flowing → working (from anything except Done).
            if self.state != State::Working {
                self.state = State::Working;
                events.push(Event {
                    t_ms: now_ms,
                    state: State::Working,
                    confidence: Confidence::Direct,
                    matched_pattern: None,
                    exit_code: None,
                });
            }
            // BEL → immediate attention on top of working.
            if self.bell_pending && self.adapter.bell_means_attention {
                self.bell_pending = false;
                let tail = tail_lines(&self.text, 3);
                let next = match self.adapter.match_question(&tail) {
                    Some(pattern) => Event {
                        t_ms: now_ms,
                        state: State::Question,
                        confidence: Confidence::Inferred {
                            rule: "bell+prompt-shape".to_string(),
                        },
                        matched_pattern: Some(pattern.to_string()),
                        exit_code: None,
                    },
                    None => Event {
                        t_ms: now_ms,
                        state: State::Blocked,
                        confidence: Confidence::Inferred {
                            rule: "bell".to_string(),
                        },
                        matched_pattern: None,
                        exit_code: None,
                    },
                };
                self.state = next.state;
                events.push(next);
            }
            return events;
        }
        // Empty feed = clock tick: evaluate silence transitions.
        events.extend(self.tick(now_ms));
        events
    }

    /// Evaluate silence-driven transitions at `now_ms` (also called by
    /// `feed(b"", now_ms)`).
    pub fn tick(&mut self, now_ms: u64) -> Vec<Event> {
        let mut events = Vec::new();
        if self.exited {
            return events;
        }
        let Some(last) = self.last_output_ms else {
            return events; // never saw output → stay unknown, never lie
        };
        let silent = now_ms.saturating_sub(last);
        let tail = tail_lines(&self.text, 3);
        // Priority: question > blocked > idle (most actionable first).
        if silent >= self.adapter.question_after_ms {
            if let Some(pattern) = self.adapter.match_question(&tail) {
                if self.state != State::Question {
                    self.state = State::Question;
                    events.push(Event {
                        t_ms: now_ms,
                        state: State::Question,
                        confidence: Confidence::Inferred {
                            rule: "silence+prompt-shape".to_string(),
                        },
                        matched_pattern: Some(pattern.to_string()),
                        exit_code: None,
                    });
                    return events;
                }
                return events;
            }
        }
        if self.error_armed && silent >= self.adapter.blocked_after_ms {
            if self.state != State::Blocked {
                self.state = State::Blocked;
                events.push(Event {
                    t_ms: now_ms,
                    state: State::Blocked,
                    confidence: Confidence::Inferred {
                        rule: "error-shape+silence".to_string(),
                    },
                    matched_pattern: None,
                    exit_code: None,
                });
                return events;
            }
            return events;
        }
        if silent >= self.adapter.idle_after_ms && self.state != State::Idle {
            self.state = State::Idle;
            events.push(Event {
                t_ms: now_ms,
                state: State::Idle,
                confidence: Confidence::Inferred {
                    rule: "silence".to_string(),
                },
                matched_pattern: None,
                exit_code: None,
            });
        }
        events
    }

    /// Report child exit at `now_ms`. Always emits `Done` (when enabled).
    pub fn child_exited(&mut self, code: i32, now_ms: u64) -> Vec<Event> {
        if self.exited || !self.adapter.done_on_exit {
            return Vec::new();
        }
        self.exited = true;
        self.state = State::Done;
        vec![Event {
            t_ms: now_ms,
            state: State::Done,
            confidence: Confidence::Direct,
            matched_pattern: None,
            exit_code: Some(code),
        }]
    }
}
