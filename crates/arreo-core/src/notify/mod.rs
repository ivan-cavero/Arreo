//! Notification rules (T-0093): **which agent transitions are worth telling the
//! operator about, and which are noise.**
//!
//! The state engine (T-0004) already knows when an agent becomes blocked, asks a
//! question or finishes, and the audit log (T-0033) already records it. What does
//! not exist is the operator's *policy*: this module is that policy, and it is
//! [pure](Policy::decide) — a transition and a little history in, a decision out,
//! no clock, no disk, no daemon. Every boundary is therefore testable without
//! running anything, and there is exactly one place the policy lives.
//!
//! ## The decision order, and why it is an order
//!
//! [`Policy::decide`] asks four questions in a fixed sequence, and the sequence is
//! the design rather than an accident of the code:
//!
//! 1. **Did a rule match?** Nothing else can be answered first: a transition no
//!    rule claims is not the policy's business, and calling that "quiet hours"
//!    would be a lie in the one place the operator goes to find the truth.
//! 2. **Is it quiet hours?** A delivery-window question, asked before the ones
//!    about this specific event, so that the *count* of what quiet hours
//!    suppressed means what it says. (If the duplicate check came first, a pane
//!    that flaps all night would be counted as duplicates and the quiet-hours
//!    count would under-report the noise the window is holding back.)
//! 3. **Is the coalescing window still open?** Also a delivery question.
//! 4. **Is this the same episode we already told them about?** Last, because it
//!    is the only reason that says "this is not news" rather than "we are not
//!    delivering right now" — and an operator reading the row wants the
//!    delivery reason when there is one.
//!
//! ## What an "episode" is
//!
//! A maximal run of one pane staying in one state. `once_per_episode` means: tell
//! the operator when the run **starts**, not on every event inside it.
//!
//! The engine can and does emit the same state twice — a second bell while
//! already `Blocked`, an inference that re-matches the same prompt — so
//! `blocked → blocked` is a real event and must not be a second notification.
//! A run that *ends* and starts again is news: `blocked → working → blocked`
//! notifies twice, because the operator who answered the first question needs to
//! know the agent is asking something else now.
//!
//! The run boundary is carried by the transition's own [`Transition::from`]:
//! `from == to` means "this event did not leave the state", which is the whole of
//! the test. That keeps the rule pure — no per-pane memory to thread in beyond
//! the one fact the caller already has.
//!
//! ## The log is the memory
//!
//! [`History`] is read from the **audit log**, not from a new table: the last
//! `notify.sent` row for a pane is durable across a restart by construction, and
//! it is queryable, which is the other half of this feature ("why was I not
//! told?"). One query per transition is affordable because transitions are rare —
//! state changes, not output lines — and it means there is no second source of
//! truth for "what have I already told them".
//!
//! ## Off means off
//!
//! A daemon with no `[notify]` section notifies nothing at all (the caller passes
//! no [`Policy`]). This is a background writer that would otherwise begin
//! appending rows for every pane transition on every existing machine, which is
//! both a behaviour change nobody asked for and the sort of trail T-0033 warns
//! about ("an audit trail that records every poll is a trail nobody reads"). An
//! operator turns it on by writing the section.
//!
//! ## The row, and the push
//!
//! A decision has two outputs and they are the *same* decision: the audit row an
//! operator reads, and — for a delivery — the [`push::PushPayload`] a paired
//! device is sent (T-0117). Both are built in one place from one
//! [`Decision::Notify`], which is what makes a withheld notification impossible
//! to push by accident: the suppressed arm has no payload to give.

pub mod push;
pub mod quiet;

use crate::proto::AgentState;
use quiet::QuietHours;

/// The states worth telling an operator about when the rules do not say
/// otherwise: the two that need a human.
///
/// `Working`/`Idle` are the ambient states a pane is in almost all the time, and
/// `Done` fires on every short command — a default that notified on those would
/// train an operator to ignore notifications, which is the failure mode this
/// whole task is about. `Unknown` is "the engine has not been told yet", not news.
pub const DEFAULT_ON: &[AgentState] = &[AgentState::Blocked, AgentState::Question];

/// One state transition, as the rule sees it.
///
/// `from` and `to` are both carried because the pair is what distinguishes news
/// from a repeat (see the module docs); `reason` is the sentence a human reads,
/// and it is built by the caller — which knows about patterns, exit codes and
/// prompts — rather than by this module, which deliberately does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    pub pane: String,
    /// The machine the pane runs on. A rule can scope to one; the daemon passes
    /// this machine's name, so on a single-machine setup the field is constant
    /// and harmless (the same shape sync and the directory already use).
    pub machine: String,
    pub at_ms: u64,
    pub from: AgentState,
    pub to: AgentState,
    /// The human sentence: `blocked (inferred:silence)`, `question: Proceed?`.
    pub reason: String,
}

/// What the policy needs to know about the past, read from the log by the caller.
///
/// Two fields, because the policy asks two questions about history: "how recently
/// did I tell them something?" (coalescing) and "did I tell them about this run?"
/// (episodes). Anything else would be state the rule does not use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct History {
    /// The timestamp of the last notification **delivered** for this pane.
    pub last_notified_ms: Option<u64>,
    /// The state that notification was about.
    pub last_notified_state: Option<AgentState>,
}

/// What to do with a transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Deliver it, with the sentence to deliver.
    Notify { reason: String },
    /// Do not deliver it, with the reason — never silently, because "why was I
    /// not told?" is the question this feature creates.
    Suppressed { reason: SuppressReason },
}

impl std::fmt::Display for Decision {
    /// The sentence an operator reads — the CLI prints this verbatim, so there is
    /// one wording for a decision rather than one per caller.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Notify { reason } => write!(f, "notify — {reason}"),
            Self::Suppressed { reason } => write!(f, "suppressed: {reason}"),
        }
    }
}

impl Decision {
    /// Whether a row should be written for this decision, and under which action.
    ///
    /// The caller owns the store, but not this mapping: one place decides that
    /// both outcomes are recorded.
    #[must_use]
    pub fn is_notify(&self) -> bool {
        matches!(self, Self::Notify { .. })
    }
}

/// Why a transition was not delivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuppressReason {
    /// No rule claimed this transition — the operator's policy is silent about it.
    NoRule,
    /// Inside the configured quiet window.
    Quiet { window: String },
    /// A notification for this pane went out too recently.
    Coalesced {
        last_notified_ms: u64,
        window_secs: u64,
        /// How long before this transition the last notification went out. Carried
        /// rather than derived, because the only place that can compute it is the
        /// decision itself: the detail used to print `last_notified_ms / 1000` as
        /// "…s ago", which reads a Unix time as a duration — a real row said "went
        /// out 1789400866s ago" for a gap of three seconds (found by review).
        since_ms: u64,
    },
    /// This pane is still in the state the operator was already told about.
    SameEpisode { state: AgentState },
}

impl std::fmt::Display for SuppressReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} — {}", self.word(), self.detail())
    }
}

impl SuppressReason {
    /// The one word a query filters on.
    #[must_use]
    pub fn word(&self) -> &'static str {
        match self {
            Self::NoRule => "no-rule",
            Self::Quiet { .. } => "quiet-hours",
            Self::Coalesced { .. } => "coalesced",
            Self::SameEpisode { .. } => "same-episode",
        }
    }

    /// The specifics, for the row's detail and for `arreo notify --why`.
    #[must_use]
    pub fn detail(&self) -> String {
        match self {
            Self::NoRule => "no rule matches this transition".to_string(),
            Self::Quiet { window } => format!("inside quiet hours {window}"),
            Self::Coalesced {
                window_secs,
                since_ms,
                ..
            } => format!(
                "a notification for this pane went out {}s ago, inside the {}s coalescing window",
                *since_ms as f64 / 1000.0,
                window_secs
            ),
            Self::SameEpisode { state } => format!(
                "already notified for this episode of {}",
                state_word(*state)
            ),
        }
    }
}

/// The lower-case name of a state, in the vocabulary the config and the CLI use.
#[must_use]
pub fn state_word(state: AgentState) -> &'static str {
    match state {
        AgentState::Unknown => "unknown",
        AgentState::Working => "working",
        AgentState::Idle => "idle",
        AgentState::Question => "question",
        AgentState::Blocked => "blocked",
        AgentState::Done => "done",
    }
}

/// Parse a state name from configuration.
#[must_use]
pub fn state_from_word(word: &str) -> Option<AgentState> {
    match word.trim().to_ascii_lowercase().as_str() {
        "unknown" => Some(AgentState::Unknown),
        "working" => Some(AgentState::Working),
        "idle" => Some(AgentState::Idle),
        "question" => Some(AgentState::Question),
        "blocked" => Some(AgentState::Blocked),
        "done" => Some(AgentState::Done),
        _ => None,
    }
}

/// The `state=` prefix on a notification row's `detail`.
///
/// The row format is [`detail_for`] and [`state_from_detail`], and it lives here
/// rather than in the writer because **two places read it back**: the daemon, to
/// reconstruct "what did I already tell them about this pane" from the log after a
/// restart, and `arreo notify --why`, to answer the operator. One definition, one
/// pair of tests, so a writer and a reader cannot drift into disagreeing about what
/// the log says — the failure this whole module exists to make impossible.
const DETAIL_STATE_PREFIX: &str = "state=";

/// The state a notification row was about, if it carries one.
///
/// `None` for a row written by a version that did not record one: the caller then
/// treats the history as unknown, which at worst re-notifies. A row that cannot be
/// read must never be read as "we told them about this", because that is the
/// silence an operator cannot explain.
#[must_use]
pub fn state_from_detail(detail: &str) -> Option<AgentState> {
    let rest = detail.strip_prefix(DETAIL_STATE_PREFIX)?;
    let word = rest.split(';').next()?;
    state_from_word(word)
}

// ---------------------------------------------------------------------------
// Quick actions (T-0094): the bounded vocabulary that answers a notification
// ---------------------------------------------------------------------------

/// The three quick actions a notification can carry — the whole vocabulary, and
/// exactly this: `reply <text>` sends the operator's text plus a newline
/// through the **same** send path a direct `send` takes (same per-verb trust
/// gate, same audit redaction), `skip` writes no pane bytes at all, and `kill`
/// ends the pane through the pane-kill path. A fourth action is a product
/// decision, not an implementation detail — there is deliberately no way to
/// extend this list from outside the crate.
///
/// Serde derives so the same value travels on the wire ([`crate::proto`]
/// re-uses it, see `Message::NotifyAct`) and is named in the `--json` schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotifyAction {
    /// Send the operator's text + newline through the pane's send path.
    Reply,
    /// Dismiss the notification; write no pane bytes.
    Skip,
    /// End the pane through the kill path.
    Kill,
}

impl NotifyAction {
    /// The operators' word for the action, used on the wire, in the row's
    /// `detail` (`action=reply`) and in the `--json` `actions` array.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reply => "reply",
            Self::Skip => "skip",
            Self::Kill => "kill",
        }
    }

    /// Parse an operator's action word.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word.trim().to_ascii_lowercase().as_str() {
            "reply" => Some(Self::Reply),
            "skip" => Some(Self::Skip),
            "kill" => Some(Self::Kill),
            _ => None,
        }
    }

    /// Which actions need the operator to supply text.
    #[must_use]
    pub fn needs_text(self) -> bool {
        matches!(self, Self::Reply)
    }
}

/// The bound on a quick-action reply's text, in **bytes** (T-0094): `text` is
/// at most this long, and longer is a refusal the operator sees — never a
/// truncation. Both the CLI (a usage error before the socket) and the daemon (a
/// refusal on the way in) check the same bound, so one definition cannot drift.
pub const MAX_REPLY_BYTES: usize = 4096;

/// Whether a reply's text is within the quick-action bound.
#[must_use]
pub fn reply_text_within_bound(text: &str) -> bool {
    text.len() <= MAX_REPLY_BYTES
}

/// The refusal an action gets on a pane that has since exited — the pane's
/// **state** is the authority, so the sentence is the pane's own, pinned here
/// once: the daemon replies with it (byte for byte), the audit row's `detail`
/// carries it, and the CLI keys its exit code (2) on the exact string.
pub const PANE_EXITED: &str = "the pane has exited";

/// The actions available for a pane in `state` (T-0094): a pane that is asking
/// can be answered (`reply`), dismissed (`skip`) or killed; any other state
/// still offers `skip` and `kill` — you cannot `reply` to a pane that is not
/// asking. The daemon's act gate and the notification's action list both read
/// this, so the list an operator sees and the gate that refuses are the same
/// rule.
#[must_use]
pub fn actions_for(state: AgentState) -> Vec<NotifyAction> {
    let mut actions: Vec<NotifyAction> = Vec::with_capacity(3);
    if state == AgentState::Question {
        actions.push(NotifyAction::Reply);
    }
    actions.push(NotifyAction::Skip);
    actions.push(NotifyAction::Kill);
    actions
}

/// The push a delivered notification carries, or `None` when the policy
/// withheld it (T-0117).
///
/// **This function is the gate, and it is a function rather than a branch at
/// the call site for one reason.** A push that ignored [`Policy::decide`] would
/// make the whole rules engine decorative — quiet hours, coalescing and the
/// episode rule would be reasons the operator is told about but not reasons
/// anything obeys — and the failure would look like *success* to any test that
/// only checked "a push arrived". Here the withheld arm has nothing to return,
/// so the only thing a caller can do with a suppressed decision is not push it.
///
/// Every field is taken from the same two values the audit row is built from:
/// the sentence is the decision's own `reason` (the row's `prompt`, byte for
/// byte), the state and pane are the transition's, and the actions come from
/// [`actions_for`] — the same function the daemon's act gate reads, so the list
/// a phone renders and the gate that refuses are one rule.
#[must_use]
pub fn push_payload(transition: &Transition, decision: &Decision) -> Option<push::PushPayload> {
    match decision {
        Decision::Notify { reason } => Some(push::PushPayload {
            pane: transition.pane.clone(),
            machine: transition.machine.clone(),
            state: transition.to,
            sentence: reason.clone(),
            actions: actions_for(transition.to),
            at_ms: transition.at_ms,
        }),
        // Not a delivery: nothing to push. See the doc comment — the whole
        // point is that this arm cannot be made to produce a payload.
        Decision::Suppressed { .. } => None,
    }
}

/// The action-list tail on a notification row's `detail`.
///
/// The format is [`detail_for`] (via [`detailed`]) and [`actions_from_detail`],
/// and it lives here beside the `state=` prefix for the same reason that prefix
/// does: **two places read it back** — the daemon, which wrote it, and
/// `arreo notify --why`, which answers the operator — and one definition is
/// what keeps a writer and a reader from drifting. An older client that never
/// heard of the tail still parses the row: `state_from_detail` reads the
/// `state=` prefix, and the tail simply goes unread (`actions_from_detail`
/// returns `None`; [`strip_actions`] removes it).
const DETAIL_ACTIONS_PREFIX: &str = " actions=";

/// The full notification-row `detail`: the state, the passed-through sentence,
/// then the bounded action list that answers it. The row an operator reads
/// carries its actions with it — "the notification and the answers, together".
#[must_use]
pub fn detailed(state: AgentState, rest: &str) -> String {
    format!(
        "{DETAIL_STATE_PREFIX}{}; {rest}{DETAIL_ACTIONS_PREFIX}{}",
        state_word(state),
        actions_for(state)
            .iter()
            .map(|action| action.as_str())
            .collect::<Vec<_>>()
            .join(",")
    )
}

/// The `detail` a notification row carries: the state it was about, then the
/// sentence a human reads — and, behind it, the quick actions that answer it.
#[must_use]
pub fn detail_for(state: AgentState, reason: &str) -> String {
    detailed(state, reason)
}

/// The actions a notification row carries, if it carries them.
///
/// `None` for a row written by a version that did not record the list: the
/// caller then treats the actions as unknown, which is the honest answer for a
/// row old enough to predate the feature. A row that cannot be read must never
/// be read as "no actions".
#[must_use]
pub fn actions_from_detail(detail: &str) -> Option<Vec<NotifyAction>> {
    let list = detail.rsplit_once(DETAIL_ACTIONS_PREFIX)?.1;
    let mut actions = Vec::new();
    for word in list.split(',') {
        actions.push(NotifyAction::parse(word)?);
    }
    Some(actions)
}

/// A notification row's `detail` without the action-list tail.
///
/// The tail lives at the end of the row's sentence, so a reader that parses the
/// rest (the CLI's `--why` reason split, for one) must see the row exactly as
/// it would have read before the feature existed — the tail says nothing about
/// the reason, and must not leak into it.
#[must_use]
pub fn strip_actions(detail: &str) -> &str {
    match detail.rsplit_once(DETAIL_ACTIONS_PREFIX) {
        Some((head, _)) => head,
        None => detail,
    }
}

/// A pane-id pattern with exactly one wildcard: `*`.
///
/// Written here rather than pulled in, and deliberately minimal: the use is
/// "which panes does this rule cover" — `build-*`, `agent-?-west` is not a thing
/// anybody writes — and a glob crate would be a dependency for a matcher that is
/// twenty lines. `*` matches any run of characters including none, so `build-*`
/// covers `build-1` and `build-`; a pattern with no `*` must match exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Glob(String);

impl Glob {
    #[must_use]
    pub fn new(pattern: &str) -> Self {
        Self(pattern.to_string())
    }

    /// Does `text` match? Iterative two-pointer with a backtrack point, so a
    /// pattern like `*-*-*-x` against a long non-matching id cannot blow up.
    #[must_use]
    pub fn matches(&self, text: &str) -> bool {
        let pattern: Vec<char> = self.0.chars().collect();
        let text: Vec<char> = text.chars().collect();
        let (mut p, mut t) = (0usize, 0usize);
        let mut star: Option<(usize, usize)> = None;
        while t < text.len() {
            if p < pattern.len() && pattern[p] == text[t] {
                p += 1;
                t += 1;
            } else if p < pattern.len() && pattern[p] == '*' {
                star = Some((p, t));
                p += 1;
            } else if let Some((sp, st)) = star {
                p = sp + 1;
                t = st + 1;
                star = Some((sp, st + 1));
            } else {
                return false;
            }
        }
        while p < pattern.len() && pattern[p] == '*' {
            p += 1;
        }
        p == pattern.len()
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One rule: what it matches.
///
/// A field that is `None` does not constrain. An empty `on` matches nothing,
/// which is how an operator disables a rule without deleting it — and it is
/// refused at load time rather than silently matching everything, because
/// "empty means all" is the reading that would notify on every transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// The states this rule is *about* (the transition's destination).
    pub on: Vec<AgentState>,
    /// Which panes, by id pattern. `None` = any pane.
    pub panes: Option<Glob>,
    /// Which machine. `None` = any machine.
    pub machine: Option<String>,
}

impl Rule {
    /// A rule that claims every pane and every machine for `on`.
    #[must_use]
    pub fn for_states(on: &[AgentState]) -> Self {
        Self {
            on: on.to_vec(),
            panes: None,
            machine: None,
        }
    }

    #[must_use]
    pub fn matches(&self, transition: &Transition) -> bool {
        if !self.on.contains(&transition.to) {
            return false;
        }
        if let Some(panes) = &self.panes {
            if !panes.matches(&transition.pane) {
                return false;
            }
        }
        if let Some(machine) = &self.machine {
            if machine != &transition.machine {
                return false;
            }
        }
        true
    }
}

/// The whole policy: the rules, and the three modifiers that decide *delivery*
/// rather than *relevance*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    /// Checked in order; the first match decides relevance. (Relevance is a
    /// yes/no, so "first match wins" is the same as "any match" — the order
    /// matters only once rules carry per-rule delivery settings, which they
    /// deliberately do not.)
    pub rules: Vec<Rule>,
    /// Tell the operator once per episode rather than once per event.
    pub once_per_episode: bool,
    /// Suppress a notification for a pane within this many seconds of the last
    /// one delivered for it. `0` = no coalescing.
    pub coalesce_secs: u64,
    /// The quiet window, in local wall-clock time.
    pub quiet: Option<QuietHours>,
    /// Minutes from UTC to local: see [`quiet`]'s module docs for why this is an
    /// offset rather than a time zone.
    pub utc_offset_minutes: i32,
}

impl Default for Policy {
    /// The useful default for a `[notify]` section that says little: the two
    /// states that need a human, once per episode, no coalescing, no quiet hours.
    fn default() -> Self {
        Self {
            rules: vec![Rule::for_states(DEFAULT_ON)],
            once_per_episode: true,
            coalesce_secs: 0,
            quiet: None,
            utc_offset_minutes: 0,
        }
    }
}

impl Policy {
    /// **The decision.** Pure: no clock (the transition carries its own
    /// timestamp), no I/O, no interior mutability.
    ///
    /// The order of the four questions is documented on the module — read that
    /// before reordering them, because each position is a deliberate answer to
    /// "which reason does the operator see, and what does the count mean?".
    #[must_use]
    pub fn decide(&self, transition: &Transition, history: &History) -> Decision {
        // 1. Is this transition any of the policy's business?
        if !self.rules.iter().any(|rule| rule.matches(transition)) {
            return Decision::Suppressed {
                reason: SuppressReason::NoRule,
            };
        }
        // 2. The quiet window.
        if let Some(quiet) = &self.quiet {
            if quiet.contains(transition.at_ms, self.utc_offset_minutes) {
                return Decision::Suppressed {
                    reason: SuppressReason::Quiet {
                        window: quiet.window(),
                    },
                };
            }
        }
        // 3. The coalescing window. The edge is inclusive of the gap: a
        //    notification exactly `coalesce_secs` after the last one goes out,
        //    because the operator asked for a *window*, not for a window and a
        //    tick of slack.
        if self.coalesce_secs > 0 {
            if let Some(last) = history.last_notified_ms {
                let window_ms = self.coalesce_secs.saturating_mul(1000);
                let elapsed = transition.at_ms.saturating_sub(last);
                if elapsed < window_ms {
                    return Decision::Suppressed {
                        reason: SuppressReason::Coalesced {
                            last_notified_ms: last,
                            window_secs: self.coalesce_secs,
                            since_ms: elapsed,
                        },
                    };
                }
            }
        }
        // 4. Is this the same episode we already told them about?
        if self.once_per_episode
            && transition.from == transition.to
            && history.last_notified_state == Some(transition.to)
        {
            return Decision::Suppressed {
                reason: SuppressReason::SameEpisode {
                    state: transition.to,
                },
            };
        }
        Decision::Notify {
            reason: transition.reason.clone(),
        }
    }
}

/// What can be wrong with a `[notify]` section.
///
/// Typed and specific because every one of these is a *configuration* mistake an
/// operator can fix, and the message names the field and the accepted values —
/// "invalid configuration" would send them to the docs for a typo.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    #[error("cannot read {path}: {detail}")]
    Io { path: String, detail: String },
    #[error("cannot parse {path}: {detail}")]
    Parse { path: String, detail: String },
    #[error("[notify] on = {unknown:?} is not a state; expected one or more of unknown, working, idle, question, blocked, done")]
    UnknownState { unknown: String },
    #[error("[notify] rules[{index}] on is empty: a rule with no states matches nothing, so delete the rule instead")]
    EmptyRule { index: usize },
    #[error("[notify] quiet_hours: {detail}")]
    QuietHours { detail: String },
    #[error("[notify] coalesce_secs = {secs} is too large (at most {max})")]
    CoalesceTooLarge { secs: u64, max: u64 },
}

/// The `[notify]` section as written, before validation.
///
/// Its own struct rather than a field on `relay::config::ConfigFile`, so the
/// section's shape and its validation live with the rules that use them — the
/// same way `[tui]`'s settings live in `arreo_tui` where they are consumed. The
/// file is read once per loader, which is what every other section already does.
#[derive(Debug, serde::Deserialize)]
struct NotifyFile {
    #[serde(default)]
    notify: Option<NotifySection>,
}

/// The section's raw fields. Every one is optional: a section may say only
/// `[notify]` and get the [`Policy::default`] behaviour, or override any part.
#[derive(Debug, serde::Deserialize)]
struct NotifySection {
    #[serde(default)]
    on: Option<Vec<String>>,
    #[serde(default)]
    rules: Option<Vec<RuleSection>>,
    #[serde(default)]
    once_per_episode: Option<bool>,
    #[serde(default)]
    coalesce_secs: Option<u64>,
    #[serde(default)]
    quiet_hours: Option<String>,
    #[serde(default)]
    utc_offset_minutes: Option<i32>,
}

#[derive(Debug, serde::Deserialize)]
struct RuleSection {
    #[serde(default)]
    on: Option<Vec<String>>,
    #[serde(default)]
    panes: Option<String>,
    #[serde(default)]
    machine: Option<String>,
}

/// The ceiling on `coalesce_secs`, a week.
///
/// A bound rather than a guess: the value is multiplied by 1000 for the window,
/// so an operator who types a stray digit (`600000000`) would otherwise get a
/// window that never reopens. Refusing is kinder than a silence they cannot
/// explain — and "why was I not told?" is exactly the question this feature
/// exists to answer.
const MAX_COALESCE_SECS: u64 = 7 * 24 * 60 * 60;

impl Policy {
    /// Read the `[notify]` section of the configuration file, if it is there.
    ///
    /// **`Ok(None)` is "notifications are off"**, and it covers the two ways an
    /// operator says so: no file, or no `[notify]` section in it. Anything else —
    /// a file that does not parse, an unknown state name, a malformed window — is
    /// a loud error, because an operator who asked for notifications and silently
    /// did not get them has a bug they cannot see.
    pub fn load(path: &std::path::Path) -> Result<Option<Self>, PolicyError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(PolicyError::Io {
                    path: path.display().to_string(),
                    detail: e.to_string(),
                })
            }
        };
        let parsed: NotifyFile = toml::from_str(&text).map_err(|e| PolicyError::Parse {
            path: path.display().to_string(),
            detail: e.to_string(),
        })?;
        let Some(section) = parsed.notify else {
            return Ok(None);
        };
        Self::from_section(&section).map(Some)
    }

    /// Validate a parsed section into a policy.
    fn from_section(section: &NotifySection) -> Result<Self, PolicyError> {
        let states = |words: &[String]| -> Result<Vec<AgentState>, PolicyError> {
            words
                .iter()
                .map(|word| {
                    state_from_word(word).ok_or_else(|| PolicyError::UnknownState {
                        unknown: word.clone(),
                    })
                })
                .collect()
        };

        // `[[notify.rules]]` replaces the default rule entirely; the flat `on`
        // is the shorthand for "one rule, no scoping", and using both is a
        // configuration the operator did not mean — so it is refused rather than
        // silently letting one win.
        let rules = match (&section.rules, &section.on) {
            (Some(rules), None) => rules
                .iter()
                .enumerate()
                .map(|(index, rule)| {
                    let on = match &rule.on {
                        Some(words) => states(words)?,
                        None => DEFAULT_ON.to_vec(),
                    };
                    if on.is_empty() {
                        return Err(PolicyError::EmptyRule { index });
                    }
                    Ok(Rule {
                        on,
                        panes: rule.panes.as_deref().map(Glob::new),
                        machine: rule.machine.clone(),
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
            (Some(_), Some(_)) => {
                return Err(PolicyError::Parse {
                    path: "configuration".to_string(),
                    detail:
                        "[notify] sets both `on` and `[[notify.rules]]`; `on` is the shorthand \
                             for a single unscoped rule, so use one or the other"
                            .to_string(),
                })
            }
            (None, Some(words)) => {
                let on = states(words)?;
                if on.is_empty() {
                    return Err(PolicyError::EmptyRule { index: 0 });
                }
                vec![Rule::for_states(&on)]
            }
            (None, None) => vec![Rule::for_states(DEFAULT_ON)],
        };

        let quiet = match &section.quiet_hours {
            Some(spec) => Some(
                QuietHours::parse(spec).map_err(|e| PolicyError::QuietHours {
                    detail: e.to_string(),
                })?,
            ),
            None => None,
        };
        let coalesce_secs = section.coalesce_secs.unwrap_or(0);
        if coalesce_secs > MAX_COALESCE_SECS {
            return Err(PolicyError::CoalesceTooLarge {
                secs: coalesce_secs,
                max: MAX_COALESCE_SECS,
            });
        }
        Ok(Self {
            rules,
            once_per_episode: section.once_per_episode.unwrap_or(true),
            coalesce_secs,
            quiet,
            utc_offset_minutes: section.utc_offset_minutes.unwrap_or(0),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(pane: &str, from: AgentState, to: AgentState, at_ms: u64) -> Transition {
        Transition {
            pane: pane.to_string(),
            machine: "workbox".to_string(),
            at_ms,
            from,
            to,
            reason: format!("{} ({})", state_word(to), state_word(from)),
        }
    }

    /// Default history: nothing has been delivered yet.
    fn fresh() -> History {
        History::default()
    }

    /// The default policy is the useful one: the two states that need a human,
    /// once per episode, and **nothing else** — the first thing to pin, because
    /// a default that notified on `working` would train an operator to ignore it.
    #[test]
    fn the_default_policy_is_the_two_states_that_need_a_human() {
        let policy = Policy::default();
        for (state, want) in [
            (AgentState::Blocked, true),
            (AgentState::Question, true),
            (AgentState::Working, false),
            (AgentState::Idle, false),
            (AgentState::Done, false),
            (AgentState::Unknown, false),
        ] {
            let decision = policy.decide(&at("p", AgentState::Working, state, 1_000), &fresh());
            assert_eq!(
                decision.is_notify(),
                want,
                "{state:?} → {decision:?} (note: a transition *from* Working *to* Working is the \
                 same state and is not a transition at all)"
            );
        }
    }

    /// **"No rule" is answered as itself**, never as quiet hours or coalescing:
    /// this is the row an operator reads to learn their config has a hole in it.
    #[test]
    fn a_transition_no_rule_claims_says_so() {
        let policy = Policy::default();
        let decision = policy.decide(
            &at("p", AgentState::Blocked, AgentState::Done, 1_000),
            &fresh(),
        );
        assert_eq!(
            decision,
            Decision::Suppressed {
                reason: SuppressReason::NoRule
            }
        );
        assert_eq!(decision_word(&decision), "no-rule");
    }

    /// Rules scope by pane pattern and by machine, and an unset field does not
    /// constrain.
    #[test]
    fn rules_scope_by_pane_and_machine() {
        let policy = Policy {
            rules: vec![Rule {
                on: vec![AgentState::Blocked],
                panes: Some(Glob::new("build-*")),
                machine: Some("workbox".to_string()),
            }],
            ..Policy::default()
        };
        let blocked = |pane: &str, machine: &str| {
            let mut t = at(pane, AgentState::Working, AgentState::Blocked, 1_000);
            t.machine = machine.to_string();
            policy.decide(&t, &fresh()).is_notify()
        };
        assert!(blocked("build-1", "workbox"), "the pattern matches");
        assert!(blocked("build-", "workbox"), "* matches nothing too");
        assert!(!blocked("build1", "workbox"), "the dash is required");
        assert!(!blocked("review", "workbox"), "another pane");
        assert!(!blocked("build-1", "pi"), "another machine");
        // And `on` is the destination, not the origin.
        let from_blocked = policy.decide(
            &at("build-1", AgentState::Blocked, AgentState::Working, 1_000),
            &fresh(),
        );
        assert_eq!(decision_word(&from_blocked), "no-rule");
    }

    /// **The episode rule's two halves, from the criteria**: a
    /// blocked→working→blocked cycle notifies twice, a flap inside one episode
    /// notifies once.
    #[test]
    fn once_per_episode_tells_the_start_of_a_run_and_not_its_inside() {
        let policy = Policy::default();
        let t = |from, to, ms| at("p", from, to, ms);

        // First blocked: delivered.
        let first = policy.decide(
            &t(AgentState::Working, AgentState::Blocked, 1_000),
            &fresh(),
        );
        assert!(first.is_notify(), "{first:?}");

        // The engine re-emits blocked while the pane is still blocked: not news.
        let flap = policy.decide(
            &t(AgentState::Blocked, AgentState::Blocked, 1_100),
            &History {
                last_notified_ms: Some(1_000),
                last_notified_state: Some(AgentState::Blocked),
            },
        );
        assert_eq!(decision_word(&flap), "same-episode");

        // The run ends and starts again: news, because the operator who answered
        // the first question needs to know the agent is asking something else.
        let again = policy.decide(
            &t(AgentState::Working, AgentState::Blocked, 2_000),
            &History {
                last_notified_ms: Some(1_000),
                last_notified_state: Some(AgentState::Blocked),
            },
        );
        assert!(again.is_notify(), "{again:?}");
    }

    /// With the rule off, a same-state event is delivered again — the difference
    /// the flag makes, asserted rather than described.
    #[test]
    fn without_once_per_episode_a_flap_is_delivered_again() {
        let policy = Policy {
            once_per_episode: false,
            ..Policy::default()
        };
        let decision = policy.decide(
            &at("p", AgentState::Blocked, AgentState::Blocked, 1_100),
            &History {
                last_notified_ms: Some(1_000),
                last_notified_state: Some(AgentState::Blocked),
            },
        );
        assert!(decision.is_notify(), "{decision:?}");
    }

    /// **The coalescing window's edge**, asserted rather than sampled: exactly at
    /// the window the notification goes out, one millisecond inside it does not.
    #[test]
    fn the_coalesce_window_opens_at_its_edge() {
        let policy = Policy {
            coalesce_secs: 60,
            ..Policy::default()
        };
        let history = History {
            last_notified_ms: Some(1_000_000),
            last_notified_state: Some(AgentState::Blocked),
        };
        let blocked_at = |ms| {
            policy.decide(
                &at("p", AgentState::Working, AgentState::Blocked, ms),
                &history,
            )
        };

        // One millisecond inside: held.
        assert_eq!(decision_word(&blocked_at(1_000_000 + 59_999)), "coalesced");
        // Exactly at the edge: delivered. (A window, not a window plus slack.)
        assert!(blocked_at(1_000_000 + 60_000).is_notify());
        assert!(blocked_at(1_000_000 + 60_001).is_notify());
    }

    /// **The quiet window's two edges** through the policy, so the wiring between
    /// the rule and the clock arithmetic is pinned too — the unit tests in
    /// [`quiet`] check the arithmetic, this checks that it is consulted.
    #[test]
    fn quiet_hours_hold_a_notification_at_both_edges_of_the_window() {
        let policy = Policy {
            quiet: Some(QuietHours::parse("22:00-07:00").expect("a window")),
            ..Policy::default()
        };
        // 02:00 UTC is inside 22:00-07:00.
        let at_0200 = 1_768_442_400_000u64;
        let inside = policy.decide(
            &at("p", AgentState::Working, AgentState::Blocked, at_0200),
            &fresh(),
        );
        assert_eq!(decision_word(&inside), "quiet-hours");
        assert!(
            inside.to_string().contains("22:00-07:00"),
            "the row names the window that held it"
        );

        // 12:00 UTC is outside it.
        let outside = policy.decide(
            &at(
                "p",
                AgentState::Working,
                AgentState::Blocked,
                at_0200 + 10 * 3_600_000,
            ),
            &fresh(),
        );
        assert!(outside.is_notify(), "{outside:?}");

        // And the offset moves both: 09:00 UTC is 04:00 in New York, inside.
        let nine = at_0200 + 7 * 3_600_000;
        let shifted = Policy {
            utc_offset_minutes: -300,
            ..policy.clone()
        };
        assert_eq!(
            decision_word(&shifted.decide(
                &at("p", AgentState::Working, AgentState::Blocked, nine),
                &fresh()
            )),
            "quiet-hours"
        );
        assert!(policy
            .decide(
                &at("p", AgentState::Working, AgentState::Blocked, nine),
                &fresh()
            )
            .is_notify());
    }

    /// **The order of the four questions**, because each position is a decision:
    /// quiet hours is asked before the episode check, so a pane that flaps all
    /// night is counted as *held by the window* rather than as duplicates — which
    /// is what makes the quiet-hours count mean "the noise I asked you to hold".
    #[test]
    fn the_reasons_come_back_in_the_documented_order() {
        let policy = Policy {
            quiet: Some(QuietHours::parse("22:00-07:00").expect("a window")),
            coalesce_secs: 60,
            ..Policy::default()
        };
        let at_0200 = 1_768_442_400_000u64;
        // Everything applies at once: no-rule loses (a rule matched), quiet beats
        // coalescing, and both beat the episode check.
        let everything = policy.decide(
            &at("p", AgentState::Blocked, AgentState::Blocked, at_0200),
            &History {
                last_notified_ms: Some(at_0200 - 1_000),
                last_notified_state: Some(AgentState::Blocked),
            },
        );
        assert_eq!(decision_word(&everything), "quiet-hours");

        // Outside the window, coalescing beats the episode check.
        let quiet = Policy {
            quiet: None,
            ..policy.clone()
        };
        let coalesced = quiet.decide(
            &at("p", AgentState::Blocked, AgentState::Blocked, at_0200),
            &History {
                last_notified_ms: Some(at_0200 - 1_000),
                last_notified_state: Some(AgentState::Blocked),
            },
        );
        assert_eq!(decision_word(&coalesced), "coalesced");

        // And with no delivery window in the way, the episode reason is reached.
        let plain = Policy::default();
        let episode = plain.decide(
            &at("p", AgentState::Blocked, AgentState::Blocked, at_0200),
            &History {
                last_notified_ms: Some(at_0200 - 1_000_000),
                last_notified_state: Some(AgentState::Blocked),
            },
        );
        assert_eq!(decision_word(&episode), "same-episode");
    }

    /// The one wildcard, including the cases a naive matcher gets wrong: a
    /// trailing star matching nothing, an interior star, and a pattern with none
    /// at all being an exact match.
    #[test]
    fn the_glob_has_one_wildcard_and_matches_the_whole_id() {
        for (pattern, text, want) in [
            ("build-*", "build-1", true),
            ("build-*", "build-", true),
            ("build-*", "build", false),
            ("build-*", "prebuild-1", false),
            ("*", "anything", true),
            ("*", "", true),
            ("*-west", "agent-1-west", true),
            ("*-west", "agent-1-west-2", false),
            ("a*c*e", "abcde", true),
            ("a*c*e", "abc", false),
            ("exact", "exact", true),
            ("exact", "exactly", false),
            ("exact", "", false),
            // The backtracking case: a long text with stars to re-try.
            ("*-*-*-x", "aaaaaaaaaaaaaaaaaaaaaaaaaay", false),
            ("*-*-*-x", "a-b-c-x", true),
        ] {
            assert_eq!(
                Glob::new(pattern).matches(text),
                want,
                "{pattern:?} against {text:?}"
            );
        }
    }

    /// State names round-trip through the words the config and the CLI use, and
    /// an unknown word is `None` rather than a guess.
    #[test]
    fn state_names_round_trip_and_unknown_is_none() {
        for state in [
            AgentState::Unknown,
            AgentState::Working,
            AgentState::Idle,
            AgentState::Question,
            AgentState::Blocked,
            AgentState::Done,
        ] {
            assert_eq!(state_from_word(state_word(state)), Some(state));
        }
        // Case and surrounding space are the operator's, not a parse error.
        assert_eq!(state_from_word("  BLOCKED "), Some(AgentState::Blocked));
        assert_eq!(state_from_word("blockd"), None);
    }

    /// A rule with no states matches nothing — refused loudly at load time rather
    /// than read as "all", which would notify on every transition.
    #[test]
    fn an_empty_rule_matches_nothing() {
        let policy = Policy {
            rules: vec![Rule::for_states(&[])],
            ..Policy::default()
        };
        let decision = policy.decide(
            &at("p", AgentState::Working, AgentState::Blocked, 1_000),
            &fresh(),
        );
        assert_eq!(decision_word(&decision), "no-rule");
        let empty = Glob::new("");
        assert!(empty.matches(""), "an empty pattern matches an empty id");
        assert!(!empty.matches("p"), "and nothing else");
    }

    /// The row format round-trips, and a row that carries no state reads as
    /// **unknown** rather than as "already told them" — the difference between a
    /// repeat notification and an unexplainable silence.
    #[test]
    fn the_notification_row_format_round_trips() {
        let detail = detail_for(AgentState::Blocked, "blocked (inferred:silence)");
        assert_eq!(
            detail,
            "state=blocked; blocked (inferred:silence) actions=skip,kill"
        );
        assert_eq!(state_from_detail(&detail), Some(AgentState::Blocked));

        for state in [
            AgentState::Unknown,
            AgentState::Working,
            AgentState::Idle,
            AgentState::Question,
            AgentState::Blocked,
            AgentState::Done,
        ] {
            assert_eq!(state_from_detail(&detail_for(state, "x")), Some(state));
        }

        // Not our shape, or a state we do not know: unknown, never a guess.
        assert_eq!(state_from_detail(""), None);
        assert_eq!(state_from_detail("blocked"), None);
        assert_eq!(state_from_detail("state=blockd; x"), None);
    }

    /// **The action list rides the notification row, and one definition serves
    /// the writer and all its readers** (T-0094): `detail_for`/`detailed` write
    /// the bounded list, `actions_from_detail` reads it back, `strip_actions`
    /// removes it for a reader that parses the rest — and an old reader that
    /// never heard of the tail (the daemon rebuilding its history, an older
    /// `--why`) still sees the `state=` prefix and the sentence exactly as it
    /// did before the feature existed.
    #[test]
    fn the_action_list_rides_the_row_and_old_readers_ignore_it() {
        for state in [
            AgentState::Unknown,
            AgentState::Working,
            AgentState::Idle,
            AgentState::Question,
            AgentState::Blocked,
            AgentState::Done,
        ] {
            let detail = detail_for(state, "sent");
            assert_eq!(
                actions_from_detail(&detail).as_deref(),
                Some(actions_for(state).as_slice()),
                "the row carries the actions for its state: {detail:?}"
            );
            // The old readers: the state prefix and the sentence are untouched
            // by the tail, and the tail itself can be stripped cleanly.
            assert_eq!(state_from_detail(&detail), Some(state));
            let word = state_word(state);
            assert_eq!(
                strip_actions(&detail),
                &format!("state={word}; sent"),
                "stripping restores the pre-feature row exactly"
            );
        }

        // A row written before the feature (no tail) carries no actions — the
        // honest answer is `None`, never an empty list pretending "no actions".
        assert_eq!(actions_from_detail("state=blocked; blocked"), None);
        assert_eq!(
            strip_actions("state=blocked; blocked"),
            "state=blocked; blocked"
        );
    }

    /// The vocabulary is exactly the three actions, and the mapping from a
    /// pane's **state** to the actions that answer it (T-0094): a `question`
    /// shows all three, anything else shows `skip` and `kill` only — you cannot
    /// `reply` to a pane that is not asking.
    #[test]
    fn the_vocabulary_is_three_actions_and_the_state_map_is_bounded() {
        use NotifyAction::{Kill, Reply, Skip};
        assert_eq!(
            actions_for(AgentState::Question),
            vec![Reply, Skip, Kill],
            "a question is answerable"
        );
        for state in [
            AgentState::Unknown,
            AgentState::Working,
            AgentState::Idle,
            AgentState::Blocked,
            AgentState::Done,
        ] {
            assert_eq!(
                actions_for(state),
                vec![Skip, Kill],
                "nothing that is not asking can be answered: {state:?}"
            );
        }

        // The operators' words round-trip, and nothing else parses.
        for action in [Reply, Skip, Kill] {
            assert_eq!(NotifyAction::parse(action.as_str()), Some(action));
        }
        assert_eq!(NotifyAction::parse("teleport"), None);
        assert_eq!(NotifyAction::parse(""), None);
        assert_eq!(NotifyAction::parse(" REPLY "), Some(Reply));

        // Only `reply` needs the operator's text.
        assert!(Reply.needs_text());
        assert!(!Skip.needs_text());
        assert!(!Kill.needs_text());
    }

    /// The reply-text bound is a **byte** bound, checked by one function that
    /// both the CLI (usage error) and the daemon (refusal) call: at most 4096
    /// bytes, and longer is a refusal, never a truncation. Multi-byte UTF-8
    /// counts by byte, not by character.
    #[test]
    fn the_reply_text_bound_is_4096_bytes() {
        assert_eq!(MAX_REPLY_BYTES, 4096);
        assert!(reply_text_within_bound(&"x".repeat(4096)));
        assert!(!reply_text_within_bound(&"x".repeat(4097)));
        // 1500 "λ" are 3000 bytes: under the byte bound while over any char
        // bound that existed; 2048 of them are exactly the bound.
        assert!(reply_text_within_bound(&"λ".repeat(1500)));
        assert!(reply_text_within_bound(&"λ".repeat(2048)));
        assert!(!reply_text_within_bound(&"λ".repeat(2049)));
    }

    /// The one-word reason, so the tests read as the thing an operator sees.
    fn decision_word(decision: &Decision) -> &'static str {
        match decision {
            Decision::Notify { .. } => "notify",
            Decision::Suppressed { reason } => reason.word(),
        }
    }

    /// **A withheld notification has no payload to push** (T-0117) — every one
    /// of the four suppress reasons, from the real policy rather than from
    /// hand-built decisions.
    ///
    /// This is the criterion "suppression stays quiet" reduced to its load-
    /// bearing form: the gate is `push_payload`, and if it ever answered `Some`
    /// for a suppression, quiet hours, coalescing and the episode rule would all
    /// become reasons the operator is *told about* but nothing obeys. The test
    /// drives each reason through the policy so a new suppress reason cannot be
    /// added without landing here.
    #[test]
    fn a_withheld_notification_has_nothing_to_push() {
        let policy = Policy {
            coalesce_secs: 60,
            quiet: Some(QuietHours::parse("22:00-07:00").expect("a window")),
            ..Policy::default()
        };
        let cases: [(&str, Transition, History); 4] = [
            // 1. No rule claims it.
            (
                "no-rule",
                at("p", AgentState::Blocked, AgentState::Working, 1_000),
                fresh(),
            ),
            // 2. Inside quiet hours (02:00 UTC is inside 22:00-07:00).
            (
                "quiet-hours",
                at("p", AgentState::Working, AgentState::Blocked, 2 * 3_600_000),
                fresh(),
            ),
            // 3. Inside the coalescing window. **12:00 UTC**, not an arbitrary
            //    millisecond count: the policy asks quiet hours before
            //    coalescing, so a case that landed at 00:00:31 would be withheld
            //    for the *window* and this row would be asserting the wrong
            //    reason.
            (
                "coalesced",
                at(
                    "p",
                    AgentState::Working,
                    AgentState::Blocked,
                    12 * 3_600_000 + 30_000,
                ),
                History {
                    last_notified_ms: Some(12 * 3_600_000),
                    last_notified_state: Some(AgentState::Working),
                },
            ),
            // 4. The same episode, told about already.
            (
                "same-episode",
                at(
                    "p",
                    AgentState::Blocked,
                    AgentState::Blocked,
                    10 * 3_600_000,
                ),
                History {
                    last_notified_ms: Some(9 * 3_600_000),
                    last_notified_state: Some(AgentState::Blocked),
                },
            ),
        ];
        for (want, transition, history) in cases {
            let decision = policy.decide(&transition, &history);
            assert_eq!(
                decision_word(&decision),
                want,
                "the policy must withhold this for {want}"
            );
            assert_eq!(
                push_payload(&transition, &decision),
                None,
                "a {want} suppression must have nothing to push"
            );
        }
    }

    /// **A delivered notification's payload carries exactly the row's facts**
    /// (T-0117): the pane, the machine, the state, the decision's own sentence
    /// (which is what the audit row writes to `prompt`), the actions
    /// [`actions_for`] gives the state, and the transition's timestamp. A phone
    /// renders the notification from this and nothing else.
    #[test]
    fn a_delivered_notification_carries_the_rows_facts() {
        let policy = Policy::default();
        let transition = Transition {
            pane: "build-1".to_string(),
            machine: "workbox".to_string(),
            at_ms: 1_789_398_272_891,
            from: AgentState::Working,
            to: AgentState::Question,
            reason: "question: Proceed? [y/n]".to_string(),
        };
        let decision = policy.decide(&transition, &fresh());
        let payload = push_payload(&transition, &decision).expect("a delivery is pushed");

        assert_eq!(payload.pane, "build-1");
        assert_eq!(payload.machine, "workbox");
        assert_eq!(payload.state, AgentState::Question);
        // The sentence is the *decision's*, which is the row's `prompt` — not a
        // second wording invented for the push.
        let Decision::Notify { reason } = &decision else {
            panic!("this transition is a delivery");
        };
        assert_eq!(&payload.sentence, reason);
        assert_eq!(payload.sentence, transition.reason);
        assert_eq!(payload.actions, actions_for(AgentState::Question));
        assert_eq!(payload.at_ms, transition.at_ms);
        // And it survives the seal a phone opens (the frame is a `Message`, so
        // the seal is the same one both doors use).
        let framed = push::encode(&payload).expect("encode");
        assert_eq!(push::decode(&framed).expect("decode"), payload);
    }

    /// A scratch config file under the repo's test scratch (never `/tmp`).
    fn config_file(tag: &str, body: &str) -> std::path::PathBuf {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-scratch/T-0093/config");
        std::fs::create_dir_all(&dir).expect("scratch");
        let path = dir.join(format!("{tag}.toml"));
        std::fs::write(&path, body).expect("write");
        path
    }

    /// **Absent means off**, both ways of saying it — a new background writer that
    /// started appending rows for every transition on every existing machine
    /// would be a behaviour change nobody asked for.
    #[test]
    fn no_section_means_notifications_are_off() {
        let missing = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-scratch/T-0093/config/does-not-exist.toml");
        assert_eq!(
            Policy::load(&missing).expect("a missing file is not an error"),
            None
        );

        let other_sections = config_file("other", "[relay]\nenabled = false\n[worktree]\n");
        assert_eq!(
            Policy::load(&other_sections).expect("a file without the section"),
            None
        );
    }

    /// The section's fields, and the shorthand relationship between `on` and
    /// `[[notify.rules]]`.
    #[test]
    fn the_section_is_read_and_validated() {
        // The bare section: the default policy.
        let bare = config_file("bare", "[notify]\n");
        let policy = Policy::load(&bare).expect("parses").expect("a policy");
        assert_eq!(policy, Policy::default());

        // Every knob.
        let full = config_file(
            "full",
            "[notify]\nonce_per_episode = false\ncoalesce_secs = 30\n\
             quiet_hours = \"22:00-07:00\"\nutc_offset_minutes = -300\n",
        );
        let policy = Policy::load(&full).expect("parses").expect("a policy");
        assert!(!policy.once_per_episode);
        assert_eq!(policy.coalesce_secs, 30);
        assert_eq!(policy.utc_offset_minutes, -300);
        assert_eq!(
            policy.quiet.map(|q| q.window()),
            Some("22:00-07:00".to_string())
        );

        // `on` is the shorthand for one unscoped rule.
        let shorthand = config_file("shorthand", "[notify]\non = [\"done\", \"idle\"]\n");
        let policy = Policy::load(&shorthand).expect("parses").expect("a policy");
        assert_eq!(policy.rules.len(), 1);
        assert_eq!(policy.rules[0].on, vec![AgentState::Done, AgentState::Idle]);
        assert_eq!(
            (
                policy.rules[0].panes.clone(),
                policy.rules[0].machine.clone()
            ),
            (None, None),
            "the shorthand scopes nothing"
        );
        // And it means exactly what it says: a state it does not name is not
        // claimed, so the operator who wrote this is not told about a question.
        let asked = policy.decide(
            &at("p", AgentState::Working, AgentState::Question, 1_000),
            &fresh(),
        );
        assert_eq!(decision_word(&asked), "no-rule");
    }

    /// Scoped rules load, and a rule that says nothing gets the default states.
    #[test]
    fn scoped_rules_load() {
        let path = config_file(
            "rules",
            "[notify]\n\n[[notify.rules]]\non = [\"blocked\"]\npanes = \"build-*\"\n\
             machine = \"workbox\"\n\n[[notify.rules]]\npanes = \"*\"\n",
        );
        let policy = Policy::load(&path).expect("parses").expect("a policy");
        assert_eq!(policy.rules.len(), 2);
        assert_eq!(
            policy.rules[0],
            Rule {
                on: vec![AgentState::Blocked],
                panes: Some(Glob::new("build-*")),
                machine: Some("workbox".to_string()),
            }
        );
        assert_eq!(
            policy.rules[1].on,
            DEFAULT_ON.to_vec(),
            "a rule that names no states is about the states worth telling a human"
        );
    }

    /// **Every configuration mistake is refused by name**, because each is a typo
    /// an operator can fix and "invalid configuration" would send them to the
    /// docs.
    #[test]
    fn configuration_mistakes_are_refused_with_the_reason() {
        let cases: [(&str, &str, &str); 6] = [
            ("unknown-state", "[notify]\non = [\"blockd\"]\n", "blockd"),
            ("empty-on", "[notify]\non = []\n", "matches nothing"),
            (
                "empty-rule",
                "[notify]\n\n[[notify.rules]]\non = []\n",
                "matches nothing",
            ),
            (
                "bad-window",
                "[notify]\nquiet_hours = \"2200-0700\"\n",
                "HH:MM-HH:MM",
            ),
            (
                "bad-hour",
                "[notify]\nquiet_hours = \"25:00-07:00\"\n",
                "hour",
            ),
            (
                "coalesce-too-large",
                "[notify]\ncoalesce_secs = 99999999999\n",
                "too large",
            ),
        ];
        for (tag, body, needle) in cases {
            let path = config_file(tag, body);
            let err = Policy::load(&path).expect_err(tag);
            assert!(
                err.to_string().contains(needle),
                "{tag}: {err} should mention {needle:?}"
            );
        }

        // And using both shapes at once is refused rather than silently letting
        // one win.
        let both = config_file(
            "both",
            "[notify]\non = [\"blocked\"]\n\n[[notify.rules]]\non = [\"done\"]\n",
        );
        let err = Policy::load(&both).expect_err("both");
        assert!(err.to_string().contains("one or the other"), "{err}");

        // A file that does not parse is loud, not a silent default.
        let broken = config_file("broken", "[notify\non = ");
        assert!(matches!(
            Policy::load(&broken).expect_err("broken"),
            PolicyError::Parse { .. }
        ));
    }
}
