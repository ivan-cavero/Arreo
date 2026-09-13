//! Adapter registry (T-0004 draft): per-harness prompt/error shapes + timing.
//!
//! The TOML format is the contract T-0017 populates per harness ([CC],
//! Codex, Pi, opencode, Gemini). This file owns parsing + validation:
//! unknown keys are rejected (a typo'd pattern name must fail loudly, not
//! silently disable detection), empty pattern lists are rejected.
//!
//! T-0072 adds the two data keys a *harness-aware* adapter carries: the
//! `harness` id and `programs` list (which panes this adapter owns), and the
//! `[resume]` table (how a pane's harness session is pinned or continued).
//! Both are validated as strictly as the patterns: a strategy that cannot
//! work — a `{session}` in the wrong place, a `continue` argv that claims to
//! carry an id, a pattern that cannot capture one — is a loud error, never a
//! silent fallback to today's behavior.

use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("adapter io: {0}")]
    Io(#[from] std::io::Error),
    #[error("adapter parse: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("adapter invalid: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAdapter {
    #[serde(default = "default_idle")]
    idle_after_ms: u64,
    #[serde(default = "default_question")]
    question_after_ms: u64,
    #[serde(default = "default_blocked")]
    blocked_after_ms: u64,
    #[serde(default = "default_true")]
    bell_means_attention: bool,
    #[serde(default = "default_true")]
    done_on_exit: bool,
    #[serde(default)]
    question_patterns: Vec<String>,
    #[serde(default)]
    error_patterns: Vec<String>,
    /// The harness this adapter speaks for, as the registry id a pane record
    /// carries (`"pi"`, `"opencode"`). Absent on the universal adapter: its
    /// panes have no harness, which is what `NULL` in `panes.harness` means.
    #[serde(default)]
    harness: Option<String>,
    /// The program names this adapter owns, matched against `argv[0]`'s file
    /// name. Empty on the universal adapter — it owns everything no other
    /// adapter claims, which is why it declares nothing.
    #[serde(default)]
    programs: Vec<String>,
    /// How to resume this harness's session (absent = no resume: restore takes
    /// the pre-T-0072 respawn+history path).
    #[serde(default)]
    resume: Option<RawResume>,
}

/// The `[resume]` table, as written in TOML.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawResume {
    kind: String,
    argv: Vec<String>,
    #[serde(default)]
    exact_argv: Option<Vec<String>>,
    #[serde(default)]
    session_pattern: Option<String>,
}

fn default_idle() -> u64 {
    2000
}
fn default_question() -> u64 {
    2000
}
fn default_blocked() -> u64 {
    2500
}
fn default_true() -> bool {
    true
}

/// The element of a resume argv template that the session id substitutes.
pub const SESSION_PLACEHOLDER: &str = "{session}";

/// How a harness's session is named and resumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeKind {
    /// Arreo chooses the id and hands it to the harness at spawn, so the
    /// session is exactly the one the record names — nothing has to be read
    /// back out of output. `argv` carries the id and is applied both at spawn
    /// and on resume (the same flags), which is why a record's args already
    /// contain it: `base_args` strips it again.
    Pin,
    /// The harness owns its ids and Arreo may never see one — pi's and
    /// opencode's interactive TUIs print nothing a caller can capture. Resuming
    /// is "continue the last session for this directory" (`argv`), with the
    /// exact session (`exact_argv`) preferred when the record carries an id.
    Continue,
}

impl ResumeKind {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "pin" => Some(Self::Pin),
            "continue" => Some(Self::Continue),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pin => "pin",
            Self::Continue => "continue",
        }
    }
}

/// A validated resume strategy (T-0072), one per harness that supports resume.
///
/// The four operations below are the whole contract: `capture` learns an id
/// from output, `base_args`/`resume_args` move between "the args as spawned"
/// and "the args without this strategy's resume argv", and the two are inverse
/// so a restore can never stack a second `--session-id` onto a record that
/// already carries one.
#[derive(Debug, Clone)]
pub struct Resume {
    kind: ResumeKind,
    argv: Vec<String>,
    exact_argv: Option<Vec<String>>,
    session_pattern: Option<String>,
    session_re: Option<regex::Regex>,
}

impl Resume {
    #[must_use]
    pub fn kind(&self) -> ResumeKind {
        self.kind
    }

    #[must_use]
    pub fn argv(&self) -> &[String] {
        &self.argv
    }

    #[must_use]
    pub fn exact_argv(&self) -> Option<&[String]> {
        self.exact_argv.as_deref()
    }

    #[must_use]
    pub fn session_pattern(&self) -> Option<&str> {
        self.session_pattern.as_deref()
    }

    /// The session id this strategy watches output for, if any: the pattern's
    /// single capture group, when it matches (T-0072 capture half).
    ///
    /// `None` is the normal case for an interactive TUI, which prints no id —
    /// the `continue` strategy resumes without one.
    #[must_use]
    pub fn capture(&self, text: &str) -> Option<String> {
        let re = self.session_re.as_ref()?;
        re.captures(text)
            .and_then(|caps| caps.get(1))
            .map(|m| m.as_str().to_string())
    }

    /// Split a spawn-args list into **the args without this strategy's resume
    /// argv** and the session those args carry, if the list was built by this
    /// strategy (`resume_args` put it there, at spawn or at an earlier
    /// restore).
    ///
    /// Inverse of [`Resume::resume_args`], and the reason a record restored
    /// twice does not grow: a `pin` pane's args already carry `--session-id`,
    /// so a second restore must strip it before re-applying, not append.
    #[must_use]
    pub fn base_args(&self, args: &[String]) -> (Vec<String>, Option<String>) {
        for template in [
            self.argv.as_slice(),
            self.exact_argv.as_deref().unwrap_or(&[]),
        ] {
            if template.is_empty() || !ends_with_template(template, args) {
                continue;
            }
            let start = args.len() - template.len();
            let session = session_slot(template, args).map(|i| args[i].clone());
            return (args[..start].to_vec(), session);
        }
        (args.to_vec(), None)
    }

    /// The argv to spawn this pane with: `args` plus the resume tokens for
    /// `session`.
    ///
    /// `session` is `None` for a `continue` strategy that has no id (resume by
    /// continuation) and for a `pin` strategy that has none either — and the
    /// latter returns `None`, because a `pin` without an id has nothing to pin
    /// and must not fabricate one.
    #[must_use]
    pub fn resume_args(&self, args: &[String], session: Option<&str>) -> Option<Vec<String>> {
        match self.kind {
            ResumeKind::Pin => {
                let id = session?;
                Some(apply_template(&self.argv, args, Some(id)))
            }
            ResumeKind::Continue => Some(match session {
                Some(id) => apply_template(
                    self.exact_argv.as_deref().unwrap_or(&self.argv),
                    args,
                    Some(id),
                ),
                None => apply_template(&self.argv, args, None),
            }),
        }
    }
}

/// True when `args` ends with `template` — the `{session}` element matches any
/// value, every other element must match exactly.
fn ends_with_template(template: &[String], args: &[String]) -> bool {
    if args.len() < template.len() {
        return false;
    }
    let tail = &args[args.len() - template.len()..];
    tail.iter()
        .zip(template)
        .all(|(a, t)| t == SESSION_PLACEHOLDER || a == t)
}

/// Where the session id sits in `args`, when `args` ends with `template` and
/// the template has a `{session}` element.
fn session_slot(template: &[String], args: &[String]) -> Option<usize> {
    let slot = template.iter().position(|e| e == SESSION_PLACEHOLDER)?;
    ends_with_template(template, args).then(|| args.len() - template.len() + slot)
}

/// `args` with `template` applied: the id replaces the `{session}` element in
/// place when the list already ends with the template, otherwise the template
/// is appended. Idempotent by construction — which is what makes a restore
/// safe to run on a record a previous restore wrote.
fn apply_template(template: &[String], args: &[String], session: Option<&str>) -> Vec<String> {
    let mut out = args.to_vec();
    match session_slot(template, args) {
        Some(slot) => {
            if let Some(id) = session {
                out[slot] = id.to_string();
            }
        }
        None => out.extend(template.iter().map(|element| match session {
            Some(id) if element == SESSION_PLACEHOLDER => id.to_string(),
            _ => element.clone(),
        })),
    }
    out
}

/// Validated adapter: compiled regexes + timing thresholds.
#[derive(Debug, Clone)]
pub struct Adapter {
    pub idle_after_ms: u64,
    pub question_after_ms: u64,
    pub blocked_after_ms: u64,
    pub bell_means_attention: bool,
    pub done_on_exit: bool,
    pub question_patterns: Vec<String>,
    pub error_patterns: Vec<String>,
    /// The harness id this adapter speaks for, if any (see `RawAdapter`).
    pub harness: Option<String>,
    /// The `argv[0]` basenames this adapter owns, if any.
    pub programs: Vec<String>,
    /// The harness's resume strategy, if it has one.
    pub resume: Option<Resume>,
    question_res: Vec<regex::Regex>,
    error_res: Vec<regex::Regex>,
}

impl Adapter {
    /// Parse + validate TOML text.
    pub fn from_toml(text: &str) -> Result<Self, AdapterError> {
        let raw: RawAdapter = toml::from_str(text)?;
        Self::validated(raw)
    }

    /// Load from a file (e.g. `adapters/default.toml`).
    pub fn load(path: &std::path::Path) -> Result<Self, AdapterError> {
        Self::from_toml(&std::fs::read_to_string(path)?)
    }

    /// Alias used by tests for readability.
    pub fn from_toml_file(path: &std::path::Path) -> Result<Self, AdapterError> {
        Self::load(path)
    }

    /// The harness id a pane running this adapter's program is recorded under.
    #[must_use]
    pub fn harness_id(&self) -> Option<&str> {
        self.harness.as_deref()
    }

    /// Whether output can teach this adapter a session id (the `pin` strategy
    /// needs no capture, but may still declare a pattern as a check).
    #[must_use]
    pub fn captures_session(&self) -> bool {
        self.resume
            .as_ref()
            .is_some_and(|r| r.session_pattern.is_some() && r.session_re.is_some())
    }

    fn validated(raw: RawAdapter) -> Result<Self, AdapterError> {
        if raw.question_patterns.is_empty() {
            return Err(AdapterError::Invalid(
                "question_patterns must not be empty".to_string(),
            ));
        }
        if raw.error_patterns.is_empty() {
            return Err(AdapterError::Invalid(
                "error_patterns must not be empty".to_string(),
            ));
        }
        if raw.idle_after_ms == 0 || raw.question_after_ms == 0 || raw.blocked_after_ms == 0 {
            return Err(AdapterError::Invalid(
                "timing thresholds must be non-zero".to_string(),
            ));
        }
        // A harness id is what the pane record stores and what a restore looks
        // the adapter back up by: an empty or padded one would be a record
        // nothing can resolve.
        if let Some(harness) = &raw.harness {
            if harness.is_empty() || harness.trim() != harness.as_str() {
                return Err(AdapterError::Invalid(format!(
                    "harness {harness:?} must be a non-empty, unpadded id"
                )));
            }
        }
        for program in &raw.programs {
            if program.is_empty() || program.contains('/') {
                return Err(AdapterError::Invalid(format!(
                    "programs entries are `argv[0]` basenames (no path, no empty), got {program:?}"
                )));
            }
        }
        // Both directions of the harness↔program pair are refusals, not
        // warnings: a program with no harness would put panes in the record
        // under a harness that does not exist, and a harness with no program
        // could never be selected for a pane at all (dead data claiming to be
        // a strategy).
        if raw.harness.is_some() && raw.programs.is_empty() {
            return Err(AdapterError::Invalid(
                "harness declared without programs: no pane could ever select this adapter"
                    .to_string(),
            ));
        }
        if raw.harness.is_none() && !raw.programs.is_empty() {
            return Err(AdapterError::Invalid(
                "programs declared without a harness id: a matched pane would have no harness to record"
                    .to_string(),
            ));
        }
        let resume = match &raw.resume {
            None => None,
            Some(resume) => {
                if raw.harness.is_none() {
                    return Err(AdapterError::Invalid(
                        "[resume] without a harness id: nothing in a pane record could find this strategy again"
                            .to_string(),
                    ));
                }
                Some(validated_resume(resume)?)
            }
        };
        let compile = |patterns: &[String]| -> Result<Vec<regex::Regex>, AdapterError> {
            patterns
                .iter()
                .map(|p| {
                    regex::Regex::new(p)
                        .map_err(|e| AdapterError::Invalid(format!("bad regex {p:?}: {e}")))
                })
                .collect()
        };
        let question_res = compile(&raw.question_patterns)?;
        let error_res = compile(&raw.error_patterns)?;
        Ok(Self {
            idle_after_ms: raw.idle_after_ms,
            question_after_ms: raw.question_after_ms,
            blocked_after_ms: raw.blocked_after_ms,
            bell_means_attention: raw.bell_means_attention,
            done_on_exit: raw.done_on_exit,
            question_patterns: raw.question_patterns,
            error_patterns: raw.error_patterns,
            harness: raw.harness,
            programs: raw.programs,
            resume,
            question_res,
            error_res,
        })
    }

    pub(crate) fn match_question(&self, tail: &str) -> Option<&str> {
        self.question_res
            .iter()
            .zip(self.question_patterns.iter())
            .find(|(re, _)| re.is_match(tail))
            .map(|(_, pattern)| pattern.as_str())
    }

    pub(crate) fn match_error(&self, text: &str) -> bool {
        self.error_res.iter().any(|re| re.is_match(text))
    }
}

/// Validate one `[resume]` table into a strategy, or say exactly what is wrong
/// with it.
///
/// The rules exist because each of these mistakes *looks* like a working
/// strategy and silently is not: a `continue` argv carrying an id would be
/// passed a flag the harness does not accept for it, a `pin` argv without one
/// would spawn a session Arreo cannot name again, and a partial placeholder
/// (`--session={session}`) would reach the harness literally.
fn validated_resume(raw: &RawResume) -> Result<Resume, AdapterError> {
    let kind = ResumeKind::parse(&raw.kind).ok_or_else(|| {
        AdapterError::Invalid(format!(
            "resume.kind {:?} is not a strategy (pin | continue)",
            raw.kind
        ))
    })?;
    check_template(&raw.argv, "resume.argv")?;
    if raw.argv.is_empty() {
        return Err(AdapterError::Invalid(
            "resume.argv must not be empty".to_string(),
        ));
    }
    match kind {
        ResumeKind::Pin => {
            if raw.exact_argv.is_some() {
                return Err(AdapterError::Invalid(
                    "resume.exact_argv is meaningless for kind = \"pin\": the id is always known"
                        .to_string(),
                ));
            }
            if slot_of(&raw.argv).is_none() {
                return Err(AdapterError::Invalid(format!(
                    "resume.argv for kind = \"pin\" must carry the id (a {SESSION_PLACEHOLDER} element)"
                )));
            }
        }
        ResumeKind::Continue => {
            if slot_of(&raw.argv).is_some() {
                return Err(AdapterError::Invalid(format!(
                    "resume.argv for kind = \"continue\" must not carry an id (no {SESSION_PLACEHOLDER}): the harness picks the session"
                )));
            }
            if let Some(exact) = &raw.exact_argv {
                if exact.is_empty() {
                    return Err(AdapterError::Invalid(
                        "resume.exact_argv must not be empty (omit it instead)".to_string(),
                    ));
                }
                check_template(exact, "resume.exact_argv")?;
                if slot_of(exact).is_none() {
                    return Err(AdapterError::Invalid(format!(
                        "resume.exact_argv must carry the id (a {SESSION_PLACEHOLDER} element)"
                    )));
                }
            }
        }
    }
    let session_re = match &raw.session_pattern {
        None => None,
        Some(pattern) => {
            let re = regex::Regex::new(pattern).map_err(|e| {
                AdapterError::Invalid(format!("bad regex resume.session_pattern {pattern:?}: {e}"))
            })?;
            // Exactly one group, always group 1: a pattern with two would leave
            // "which one is the id" to convention, and a pattern with none
            // could match without ever yielding a session.
            if re.captures_len() != 2 {
                return Err(AdapterError::Invalid(format!(
                    "resume.session_pattern {pattern:?} must have exactly one capture group (the id)"
                )));
            }
            Some(re)
        }
    };
    Ok(Resume {
        kind,
        argv: raw.argv.clone(),
        exact_argv: raw.exact_argv.clone(),
        session_pattern: raw.session_pattern.clone(),
        session_re,
    })
}

/// Check one argv template's elements: non-empty, and the placeholder — when
/// present — is a whole element, because a template is a list of argv words
/// and the id has to be one of them.
fn check_template(template: &[String], what: &str) -> Result<(), AdapterError> {
    for element in template {
        if element.is_empty() {
            return Err(AdapterError::Invalid(format!(
                "{what} contains an empty argv element"
            )));
        }
        if element.contains(SESSION_PLACEHOLDER) && element != SESSION_PLACEHOLDER {
            return Err(AdapterError::Invalid(format!(
                "{what} element {element:?} embeds {SESSION_PLACEHOLDER} in a larger word: it must be the whole element"
            )));
        }
    }
    if template
        .iter()
        .filter(|element| element.as_str() == SESSION_PLACEHOLDER)
        .count()
        > 1
    {
        return Err(AdapterError::Invalid(format!(
            "{what} carries more than one {SESSION_PLACEHOLDER} element"
        )));
    }
    Ok(())
}

/// The index of the `{session}` element in a validated template.
fn slot_of(template: &[String]) -> Option<usize> {
    template.iter().position(|e| e == SESSION_PLACEHOLDER)
}

impl Default for Adapter {
    fn default() -> Self {
        Self::from_toml(include_str!("../../../../adapters/default.toml"))
            .expect("default adapter is valid")
    }
}
