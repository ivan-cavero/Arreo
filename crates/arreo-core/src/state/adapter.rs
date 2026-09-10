//! Adapter registry (T-0004 draft): per-harness prompt/error shapes + timing.
//!
//! The TOML format is the contract T-0017 populates per harness (Claude Code,
//! Codex, Pi, opencode, Gemini). This file owns parsing + validation:
//! unknown keys are rejected (a typo'd pattern name must fail loudly, not
//! silently disable detection), empty pattern lists are rejected.

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

impl Default for Adapter {
    fn default() -> Self {
        Self::from_toml(include_str!("../../../../adapters/default.toml"))
            .expect("default adapter is valid")
    }
}
