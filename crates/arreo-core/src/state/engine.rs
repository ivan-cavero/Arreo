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
    /// Whether the last output contained BEL.
    bell_pending: bool,
    /// Whether an error shape was seen since the last state change.
    error_armed: bool,
    exited: bool,
}

/// Cap on retained visible text (64 KiB — tail matching only needs the end).
const TEXT_CAP: usize = 64 * 1024;

impl Engine {
    #[must_use]
    pub fn new(adapter: Adapter, _now_ms: u64) -> Self {
        Self {
            adapter,
            state: State::Unknown,
            text: String::new(),
            last_output_ms: None,
            bell_pending: false,
            error_armed: false,
            exited: false,
        }
    }

    #[must_use]
    pub fn state(&self) -> &State {
        &self.state
    }

    /// Feed raw output bytes observed at `now_ms`. Emits transitions.
    pub fn feed(&mut self, bytes: &[u8], now_ms: u64) -> Vec<Event> {
        let mut events = Vec::new();
        if self.exited {
            return events;
        }
        if !bytes.is_empty() {
            if bytes.contains(&0x07) {
                self.bell_pending = true;
            }
            let visible = strip_ansi(bytes);
            self.text.push_str(&visible);
            if self.text.len() > TEXT_CAP {
                let drop = self.text.len() - TEXT_CAP;
                self.text.drain(..drop);
            }
            if self.adapter.match_error(&visible) {
                self.error_armed = true;
            }
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
