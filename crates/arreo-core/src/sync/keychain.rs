//! The keychain bridge (T-0083): the file carries the variable's *name*, this
//! machine supplies the value.
//!
//! One sentence: what syncs is the reference, what stays is the secret — each
//! machine resolves the name from its own store and injects it into the harness
//! it spawns, and a machine that has the file but not the variable says so **by
//! name** instead of letting the harness fail with whatever the provider says.
//!
//! ## Why the value never travels, and why that is not a compromise
//!
//! ROADMAP §3.8: "a synced provider list never leaks keys to the RPi in your
//! living room". Encrypting the secret into the synced file would not fix that —
//! the key would travel with the ciphertext, which is the same leak with more
//! steps — so the file holds `${ARREO_ENV:VBK_PROD_KEY}`-shaped *references* and
//! the value is looked up locally at spawn.
//!
//! ## Why a missing variable is reported by name
//!
//! T-0075 measured this the hard way: with `{env:VBK_PROD_KEY}` in the config
//! and the variable unset, opencode does not complain about the config — it
//! makes a live request and the *provider* answers `401 missing or invalid
//! token`. The failure an operator sees is a provider error with no mention of
//! the machine they forgot to set a key on, and the machine that would be
//! correct is the one whose config arrived from a peer. So this module answers
//! the only question that matters — which names does this file need, and which
//! of them does this machine not have — as a list of names, and every refusal
//! built on it prints the name and the command that fixes it.
//!
//! ## The residual, closed here rather than in the scanner
//!
//! `fixtures::is_env_reference` cannot tell an all-uppercase literal from a
//! variable name — that is documented there, deliberately, because the bare
//! name *is* omp's only working dialect. The sync path closes the gap: a
//! reference that does not resolve to a variable on the receiving machine stops
//! the file by name. A literal that happened to look like a name therefore
//! cannot ride along silently, and a real reference on a machine that has not
//! been given the value yet is an actionable message instead of an opaque 401.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::fixtures::is_env_reference;
use crate::sync::paths::{MachineEnv, PathError};
use crate::sync::presets::Dialect;

/// The neutral spelling the *payload* carries.
///
/// ROADMAP §3.8's escape hatch, and the reason a payload is harness-agnostic:
/// the sender writes the reference in its own harness's dialect, the payload
/// holds this form, and the receiver rewrites it into *its* dialect before the
/// file lands. Both directions are pure text substitution, so comments, key
/// order and formatting survive untouched.
pub const NEUTRAL_PREFIX: &str = "${ARREO_ENV:";

/// What can go wrong reading or writing the machine's secret store.
#[derive(Debug)]
pub enum KeychainError {
    /// The store's own path could not be resolved on this machine.
    Path(PathError),
    /// The store file could not be read or written.
    Io(std::io::Error),
    /// The store file is not the JSON object it must be.
    Json(serde_json::Error),
    /// A variable name that is not a name.
    BadName(String),
}

impl std::fmt::Display for KeychainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeychainError::Path(e) => write!(f, "{e}"),
            KeychainError::Io(e) => write!(f, "secret store: {e}"),
            KeychainError::Json(e) => write!(f, "secret store is not readable JSON: {e}"),
            KeychainError::BadName(name) => write!(
                f,
                "{name:?} is not a variable name (uppercase letters, digits and underscores)"
            ),
        }
    }
}

impl std::error::Error for KeychainError {}

/// The machine's own secrets: a 0600 JSON object of `NAME` → value.
///
/// A file rather than the OS keychain because the keychain backends are
/// per-platform crates and this ticket may not add a dependency; the property
/// that matters here — the value lives on this machine and never enters a synced
/// file — holds either way, and swapping the backing store later does not change
/// this API. The path is resolved from the machine's own config root, so two
/// isolated roots in one process each get their own store.
///
/// Values are never formatted: [`SecretStore`] has no `Debug`, and
/// [`InjectionPlan`]'s redacts.
pub struct SecretStore {
    path: PathBuf,
    values: BTreeMap<String, String>,
}

impl SecretStore {
    /// The store's default path on this machine
    /// (`$XDG_CONFIG_HOME/arreo/secrets.json`).
    pub fn default_path(env: &MachineEnv) -> Result<PathBuf, KeychainError> {
        env.resolve("$XDG_CONFIG_HOME/arreo/secrets.json")
            .map_err(KeychainError::Path)
    }

    /// Open the store at `path`. A missing file is an empty store, not an
    /// error: "this machine has set no secrets yet" is the normal first run.
    pub fn open(path: PathBuf) -> Result<Self, KeychainError> {
        match std::fs::read(&path) {
            Ok(bytes) => {
                let values: BTreeMap<String, String> =
                    serde_json::from_slice(&bytes).map_err(KeychainError::Json)?;
                warn_if_readable(&path);
                Ok(Self { path, values })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self {
                path,
                values: BTreeMap::new(),
            }),
            Err(e) => Err(KeychainError::Io(e)),
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The value of `name`: this machine's store first, the process environment
    /// second.
    ///
    /// The store is authoritative because it is what `arreo sync secret set`
    /// writes; the environment is the fallback so an operator who exported the
    /// variable by hand — the design's own step 0 — still resolves.
    #[must_use]
    pub fn resolve(&self, name: &str) -> Option<String> {
        self.values
            .get(name)
            .cloned()
            .or_else(|| std::env::var(name).ok())
    }

    /// The names this machine holds, in name order.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.values.keys().cloned().collect()
    }

    /// Set `name`, writing the whole store owner-only.
    ///
    /// Written whole rather than appended because the file is a JSON object: a
    /// half-updated object is not parseable, and re-writing N small keys costs
    /// nothing next to the cost of a store that cannot be read back.
    pub fn set(&mut self, name: &str, value: &str) -> Result<(), KeychainError> {
        if !is_name(name) {
            return Err(KeychainError::BadName(name.to_string()));
        }
        self.values.insert(name.to_string(), value.to_string());
        let bytes = serde_json::to_vec_pretty(&self.values).map_err(KeychainError::Json)?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(KeychainError::Io)?;
        }
        crate::write_private_bytes(&self.path, &bytes).map_err(KeychainError::Io)
    }
}

/// Uppercase letters, digits and underscores, starting with a letter — the same
/// shape the scanner's `is_env_reference` accepts as a name, restated here
/// because this function *creates* names rather than recognising them.
fn is_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_uppercase()
        && name.len() <= 64
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Warn, loudly and once, about a store the whole user can read.
#[cfg(unix)]
fn warn_if_readable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(metadata) = std::fs::metadata(path) else {
        return;
    };
    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        eprintln!(
            "arreo: {} is mode {mode:o}; this machine's secrets are group- or world-readable. \
             `arreo sync secret set` rewrites the file owner-only, but until then treat every key \
             in it as exposed",
            path.display()
        );
    }
}

#[cfg(not(unix))]
fn warn_if_readable(_path: &Path) {}

/// The variable name a reference-shaped value names, if it names one.
///
/// Accepts every dialect the scanner accepts, plus the neutral Arreo form —
/// which the scanner deliberately does *not* accept (it is not a harness
/// reference until a machine translates it, so `scan_secrets` would flag it).
#[must_use]
pub fn reference_name(value: &str) -> Option<&str> {
    let value = value
        .trim()
        .trim_matches(|c| c == '"' || c == '\'' || c == ',' || c == ';');
    if let Some(rest) = value.strip_prefix(NEUTRAL_PREFIX) {
        let name = rest.strip_suffix('}')?;
        // A name, checked by the scanner's own predicate so the two halves of
        // T-0083 cannot disagree about what a name looks like.
        return (is_env_reference(name) && is_name(name)).then_some(name);
    }
    if !is_env_reference(value) {
        return None;
    }
    for prefix in ["{env:", "${"] {
        if let Some(rest) = value.strip_prefix(prefix) {
            return rest.strip_suffix('}');
        }
    }
    if let Some(rest) = value.strip_prefix('$') {
        return Some(rest);
    }
    // omp's dialect: the bare name (T-0080).
    Some(value)
}

/// The variable names `text` references, in file order, without repeats.
///
/// Dialect-agnostic on purpose: this is a *scan* of what the file needs, and a
/// file that mentions `${ARREO_ENV:X}` (about to be translated), `{env:X}`
/// (already native) or a bare `X` all need the same value present. Which
/// spelling is *written* is the dialect's business.
#[must_use]
pub fn references(text: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for (start, end) in value_spans(text) {
        if let Some(name) = reference_name(&text[start..end]) {
            if !names.iter().any(|seen| seen == name) {
                names.push(name.to_string());
            }
        }
    }
    names
}

/// Rewrite every native reference in `text` into the neutral Arreo form — what
/// a payload carries, so the bytes that travel are not any one machine's
/// harness spelling (`docs/harness-centralization.md` §3.2).
#[must_use]
pub fn normalize(text: &str) -> String {
    rewrite(text, &|value| {
        let name = reference_name(value)?;
        if value.trim_start().starts_with(NEUTRAL_PREFIX) {
            return None;
        }
        Some(format!("{NEUTRAL_PREFIX}{name}}}"))
    })
}

/// Rewrite every neutral reference into `dialect`'s spelling — what this
/// machine's harness needs on disk.
#[must_use]
pub fn denormalize(text: &str, dialect: Dialect) -> String {
    rewrite(text, &|value| {
        let rest = value.trim_start().strip_prefix(NEUTRAL_PREFIX)?;
        let name = rest.strip_suffix('}')?;
        // **The name is validated before it is spliced into the file's syntax**
        // (review F1). Without this check a payload could carry
        // `${ARREO_ENV:X", "mcp": {"command": ["sh","-c","id"]}, "z":"}` and the
        // substitution would write that text into the receiver's harness config
        // as *syntax* — a remote peer choosing a config key that runs a command.
        // A name is a name by construction in the neutral form, so anything else
        // here is hostile or corrupt, and leaving the bytes untouched is the only
        // safe answer: the payload then fails the receiver's reference check (the
        // value does not resolve) instead of being silently rewritten.
        if !is_name(name) {
            return None;
        }
        Some(dialect.native(name))
    })
}

/// The byte ranges of the *values* in a config text: the contents of quoted
/// strings, and bare tokens after an assignment separator.
///
/// A scanner over text rather than a parse, because two of the three file types
/// here are not JSON: `opencode.jsonc` may carry comments `serde_json` will not
/// read, and `models.yml`/`config.yml` are YAML with no parser in the workspace.
/// The spans are what [`normalize`]/[`denormalize`] substitute into, so a
/// reference is rewritten in place and everything around it — comments, key
/// order, indentation — is left exactly as the operator wrote it. The engine
/// reads the same spans for its absolute-path rule, so "a value" means one thing
/// across the two checks.
pub(crate) fn value_spans(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut index = 0;
    let mut covered = 0;
    // The last byte that was not whitespace, so a single quote can be told apart
    // from the one in `owner's`: `'…'` is a string only where a *value* starts
    // (after `:`, `=`, `[`, `,` or a YAML list dash). Getting this wrong is not
    // cosmetic — an apostrophe in a comment sent the scanner looking for a
    // closing quote past the end of the file and the whole document stopped
    // being scanned.
    let mut previous_significant = 0_u8;
    while index < bytes.len() {
        let byte = bytes[index];
        let next = bytes.get(index + 1).copied();
        match byte {
            b'"' | b'\'' if byte == b'"' || starts_a_value(previous_significant) => {
                let quote = byte;
                let start = index + 1;
                let mut cursor = start;
                while cursor < bytes.len() {
                    match bytes[cursor] {
                        b'\\' => cursor += 2,
                        c if c == quote => break,
                        _ => cursor += 1,
                    }
                }
                if start < cursor {
                    spans.push((start, cursor));
                    covered = cursor;
                }
                index = cursor + 1;
                previous_significant = 0;
                continue;
            }
            // Comments are skipped whole: a reference or a path *mentioned* in a
            // comment is not one this machine has to resolve, and rewriting
            // prose would be exactly the silent edit this feature avoids.
            b'/' if next == Some(b'/') => {
                index = line_end(bytes, index);
                previous_significant = 0;
                continue;
            }
            b'/' if next == Some(b'*') => {
                index = block_comment_end(bytes, index);
                previous_significant = 0;
                continue;
            }
            b'#' if index == 0 || bytes[index - 1].is_ascii_whitespace() => {
                index = line_end(bytes, index);
                previous_significant = 0;
                continue;
            }
            b':' | b'=' => {
                let mut cursor = index + 1;
                while cursor < bytes.len() && (bytes[cursor] == b' ' || bytes[cursor] == b'\t') {
                    cursor += 1;
                }
                if cursor >= bytes.len() || bytes[cursor] == b'"' || bytes[cursor] == b'\'' {
                    // A quoted value: the string rule above owns that span.
                    previous_significant = byte;
                    index += 1;
                    continue;
                }
                // **The neutral Arreo form is one value, braces included**
                // (review F3). The generic bare-token rule below ends a token at
                // `}`, which is right for JSON/YAML punctuation and wrong for
                // `${ARREO_ENV:NAME}` — the span came out one byte short, so the
                // form was never recognised: an unresolved reference landed
                // verbatim (the harness then 401s with no mention of the missing
                // variable) and omp's own unquoted bare-name dialect was refused
                // by the scanner on arrival. Scanning the braces as a unit fixes
                // both directions and keeps every other bare token as it was.
                if bytes[cursor..].starts_with(NEUTRAL_PREFIX.as_bytes()) {
                    if let Some(close) = bytes[cursor..].iter().position(|b| *b == b'}') {
                        let end = cursor + close + 1;
                        spans.push((cursor, end));
                        covered = end;
                        previous_significant = b'}';
                        index = end;
                        continue;
                    }
                }
                let start = cursor;
                while cursor < bytes.len()
                    && !matches!(
                        bytes[cursor],
                        b' ' | b'\t'
                            | b'\r'
                            | b'\n'
                            | b','
                            | b';'
                            | b')'
                            | b']'
                            | b'}'
                            | b'#'
                            // A quote ends a bare token so the string rule below
                            // gets the value inside it: `"plugin": ["/abs.js"]`
                            // must yield `/abs.js` as a value, or the
                            // absolute-path rule would read `["/abs.js"` and
                            // see no path at all.
                            | b'"'
                            | b'\''
                    )
                {
                    cursor += 1;
                }
                if start < cursor && start >= covered {
                    spans.push((start, cursor));
                    covered = cursor;
                }
                previous_significant = bytes[cursor.saturating_sub(1)];
                index = cursor;
                continue;
            }
            _ => {}
        }
        if !byte.is_ascii_whitespace() {
            previous_significant = byte;
        }
        index += 1;
    }
    spans
}

/// Does a single quote here open a value? Yes after the separators a config file
/// uses, and at the start of a document.
fn starts_a_value(previous: u8) -> bool {
    matches!(previous, 0 | b':' | b'=' | b'[' | b',' | b'-' | b'(')
}

/// The byte index just past the end of the line starting at `index`.
fn line_end(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len() && bytes[index] != b'\n' {
        index += 1;
    }
    index
}

/// The byte index just past the `*/` that closes the block comment at `index`.
fn block_comment_end(bytes: &[u8], mut index: usize) -> usize {
    while index + 1 < bytes.len() {
        if bytes[index] == b'*' && bytes[index + 1] == b'/' {
            return index + 2;
        }
        index += 1;
    }
    bytes.len()
}

/// Replace the value spans `f` claims, right to left so offsets stay valid.
fn rewrite(text: &str, f: &dyn Fn(&str) -> Option<String>) -> String {
    let spans = value_spans(text);
    let mut out = text.to_string();
    for (start, end) in spans.into_iter().rev() {
        let value = &text[start..end];
        if let Some(replacement) = f(value) {
            out.replace_range(start..end, &replacement);
        }
    }
    out
}

/// What a file needs from this machine, name by name.
pub struct InjectionPlan {
    machine: String,
    /// `(name, value)` for the names this machine holds. Not `Debug`, not
    /// printed: this is the only place a value exists outside the store.
    resolved: Vec<(String, String)>,
    missing: Vec<String>,
}

impl InjectionPlan {
    /// The environment entries a PTY spawn applies to the harness child.
    #[must_use]
    pub fn environment(&self) -> Vec<(String, String)> {
        self.resolved.clone()
    }

    /// The names that resolved, for a report that names them without values.
    #[must_use]
    pub fn resolved_names(&self) -> Vec<&str> {
        self.resolved
            .iter()
            .map(|(name, _)| name.as_str())
            .collect()
    }

    /// The names this machine does not have.
    #[must_use]
    pub fn missing(&self) -> &[String] {
        &self.missing
    }

    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.missing.is_empty()
    }

    /// The by-name sentence for each missing variable (T-0075's 401, turned
    /// into something actionable).
    #[must_use]
    pub fn report(&self) -> Vec<String> {
        self.missing
            .iter()
            .map(|name| {
                format!(
                    "{name} is not set on {} — the harness would answer the provider's 401 \
                     instead of a config error; set it with `arreo sync secret set {name}`",
                    self.machine
                )
            })
            .collect()
    }
}

impl std::fmt::Debug for InjectionPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InjectionPlan")
            .field("machine", &self.machine)
            .field("resolved", &self.resolved_names())
            .field("values", &"<redacted>")
            .field("missing", &self.missing)
            .finish()
    }
}

/// What `text` needs and this machine has, for a harness this machine spawns.
#[must_use]
pub fn plan(text: &str, secrets: &SecretStore, machine: &str) -> InjectionPlan {
    let mut resolved = Vec::new();
    let mut missing = Vec::new();
    for name in references(text) {
        match secrets.resolve(&name) {
            Some(value) => resolved.push((name, value)),
            None => missing.push(name),
        }
    }
    InjectionPlan {
        machine: machine.to_string(),
        resolved,
        missing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(dir: &Path) -> SecretStore {
        SecretStore::open(dir.join("secrets.json")).expect("open")
    }

    #[test]
    fn every_dialect_and_the_neutral_form_yield_the_same_name() {
        for value in [
            "{env:VBK_PROD_KEY}",
            "$VBK_PROD_KEY",
            "${VBK_PROD_KEY}",
            "VBK_PROD_KEY",
            "${ARREO_ENV:VBK_PROD_KEY}",
            "\"${ARREO_ENV:VBK_PROD_KEY}\"",
            "{env:VBK_PROD_KEY},",
        ] {
            assert_eq!(
                reference_name(value),
                Some("VBK_PROD_KEY"),
                "value {value:?}"
            );
        }
        // Not references: a lowercase literal, a model id, a URL, a number.
        for value in [
            "vbk_pro_abc",
            "deepseek-v4-flash-0731",
            "https://x/y",
            "true",
        ] {
            assert_eq!(reference_name(value), None, "value {value:?}");
        }
    }

    /// A trimmed but faithful slice of the owner's `opencode.jsonc`
    /// (`docs/harness-centralization.md` §4 step 1): one custom provider whose
    /// key is a reference, with everything else portable intent.
    const WORKED_CASE: &str = r#"{
  "$schema": "https://opencode.ai/config.json",
  "provider": {
    "verboo": {
      "name": "Verboo Code",
      "npm": "@ai-sdk/openai-compatible",
      "options": {
        "baseURL": "https://code.verboo.ai/router/v1",
        "apiKey": "{env:VBK_PROD_KEY}"
      },
      "models": {
        "deepseek-v4-flash-0731": {
          "id": "deepseek-v4-flash-0731",
          "name": "Verboo deepseek-v4-flash-0731",
          "tool_call": true,
          "limit": { "context": 1048576, "output": 65536 }
        }
      }
    }
  }
}
"#;

    #[test]
    fn the_worked_case_file_needs_exactly_one_name() {
        assert_eq!(references(WORKED_CASE), vec!["VBK_PROD_KEY".to_string()]);
        // The scanner the sync path reuses agrees: the correct configuration is
        // clean, which was T-0075's blocker.
        assert!(crate::fixtures::scan_secrets(WORKED_CASE).is_empty());
    }

    #[test]
    fn normalizing_a_file_leaves_everything_but_the_reference_alone() {
        let original = concat!(
            "{\n",
            "  // the owner's note about the provider\n",
            "  \"provider\": {\n",
            "    \"verboo\": {\n",
            "      \"name\": \"Verboo Code\",\n",
            "      \"npm\": \"@ai-sdk/openai-compatible\",\n",
            "      \"options\": {\n",
            "        \"baseURL\": \"https://code.verboo.ai/router/v1\",\n",
            "        \"apiKey\": \"{env:VBK_PROD_KEY}\"\n",
            "      }\n",
            "    }\n",
            "  }\n",
            "}\n",
        );
        let neutral = normalize(original);
        assert!(
            neutral.contains("\"apiKey\": \"${ARREO_ENV:VBK_PROD_KEY}\""),
            "normalize produced:\n{neutral}"
        );
        assert!(neutral.contains("// the owner's note about the provider"));
        assert!(neutral.contains("\"https://code.verboo.ai/router/v1\""));
        assert_eq!(references(&neutral), vec!["VBK_PROD_KEY".to_string()]);
        // And back, byte for byte, for the harness that wrote it.
        assert_eq!(denormalize(&neutral, Dialect::OpencodeEnvBrace), original);
    }

    #[test]
    fn one_neutral_reference_lands_in_each_harnesss_own_spelling() {
        let payload = "{ \"apiKey\": \"${ARREO_ENV:VBK_PROD_KEY}\" }\n";
        assert_eq!(
            denormalize(payload, Dialect::OpencodeEnvBrace),
            "{ \"apiKey\": \"{env:VBK_PROD_KEY}\" }\n"
        );
        assert_eq!(
            denormalize(payload, Dialect::PiDollar),
            "{ \"apiKey\": \"${VBK_PROD_KEY}\" }\n"
        );
        // omp takes the bare name — no sigil (T-0080).
        assert_eq!(
            denormalize(payload, Dialect::OmpBareName),
            "{ \"apiKey\": \"VBK_PROD_KEY\" }\n"
        );
    }

    #[test]
    fn an_expressed_reference_is_named_but_a_literal_key_is_not_a_name() {
        // The residual the scanner documents, closed by the name check: an
        // uppercase literal with no provider prefix *is* called a reference
        // here (that is omp's dialect), and the sync path then refuses by name
        // if the machine does not have it. A provider-prefixed literal is read
        // by find_token, which never consults this predicate.
        assert_eq!(
            reference_name("TOTALLY_A_LITERAL"),
            Some("TOTALLY_A_LITERAL")
        );
        assert_eq!(reference_name("vbk_pro_0123456789abcdef"), None);
        assert!(
            crate::fixtures::find_token("token: vbk_pro_0123456789abcdef").is_some(),
            "the measured prefix rule is what the sync path relies on"
        );
    }

    #[test]
    fn the_store_resolves_a_name_and_reports_the_ones_it_does_not_have() {
        let dir = std::env::temp_dir().join(format!("arreo-keychain-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let mut secrets = store(&dir);
        secrets.set("VBK_PROD_KEY", "value-one").expect("set");
        let file = concat!(
            "{ \"apiKey\": \"{env:VBK_PROD_KEY}\",\n",
            "  \"other\": \"${ARREO_ENV:NOT_SET_ON_THIS_MACHINE}\" }\n"
        );
        let plan = plan(file, &secrets, "workbox");
        assert_eq!(plan.resolved_names(), vec!["VBK_PROD_KEY"]);
        assert_eq!(
            plan.environment(),
            vec![("VBK_PROD_KEY".to_string(), "value-one".to_string())]
        );
        assert_eq!(plan.missing(), ["NOT_SET_ON_THIS_MACHINE"]);
        assert!(!plan.is_complete());
        let report = plan.report().join("\n");
        assert!(report.contains("NOT_SET_ON_THIS_MACHINE is not set on workbox"));
        assert!(report.contains("arreo sync secret set NOT_SET_ON_THIS_MACHINE"));
        // The plan's Debug never carries a value.
        assert!(!format!("{plan:?}").contains("value-one"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_bad_name_is_refused_before_it_reaches_the_store() {
        let dir = std::env::temp_dir().join(format!("arreo-keychain-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let mut secrets = store(&dir);
        assert!(matches!(
            secrets.set("lower-case", "x"),
            Err(KeychainError::BadName(_))
        ));
        assert!(!secrets.path().exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod reference_injection_tests {
    use super::*;

    /// **A payload cannot inject syntax through a reference** (review F1). The
    /// neutral form is a name by construction; a body carrying quotes, commas or
    /// braces is hostile, and substituting it would let a remote peer choose the
    /// receiver's harness config — including keys that run commands (`mcp`,
    /// `plugin`). The bytes are left alone, so the receiver's reference check
    /// refuses the payload by name instead of writing it.
    #[test]
    fn denormalize_never_splices_a_name_that_is_not_one() {
        let attack = r#"{"theme": "${ARREO_ENV:X", "mcp": {"evil": {"command": ["sh", "-c", "id"]}}, "z": "}"}"#;
        let out = denormalize(attack, Dialect::OpencodeEnvBrace);
        assert_eq!(
            out, attack,
            "a non-name body must not be rewritten into file syntax"
        );
        assert!(!out.contains("{env:"), "nothing was spliced: {out}");
        // The unquoted shape, which is where the span ends exactly on the `}`
        // and only the name check stands between a payload and the file's
        // syntax. A space is not a name.
        let unquoted = "token: ${ARREO_ENV:NOT A NAME}";
        assert_eq!(
            denormalize(unquoted, Dialect::OpencodeEnvBrace),
            unquoted,
            "a body that is not a name is left alone, so the reference check refuses it"
        );
        let smuggled = r#"token: ${ARREO_ENV:X", "mcp": {"command": ["sh"]}}"#;
        assert_eq!(
            denormalize(smuggled, Dialect::OpencodeEnvBrace),
            smuggled,
            "a value that only starts like a reference is left exactly as it arrived, \
             so nothing it contains can become syntax"
        );
        // The honest case still works, in every dialect.
        for (dialect, expected) in [
            (Dialect::OpencodeEnvBrace, "${ARREO_ENV:VBK_PROD_KEY}"),
            (Dialect::PiDollar, "${ARREO_ENV:VBK_PROD_KEY}"),
            (Dialect::OmpBareName, "${ARREO_ENV:VBK_PROD_KEY}"),
        ] {
            let text = format!(r#"apiKey: "{expected}""#);
            let out = denormalize(&text, dialect);
            assert!(
                out.contains(&dialect.native("VBK_PROD_KEY")),
                "{dialect:?} rewrites a real name: {out}"
            );
        }
    }

    /// **The neutral form is one value, braces included** (review F3). The
    /// bare-token rule ends a token at `}`, which left the span a byte short: an
    /// unquoted reference (idiomatic YAML, and omp's own dialect) was never
    /// recognised, so it travelled unresolved and — in the other direction — was
    /// refused by the scanner as a literal.
    #[test]
    fn an_unquoted_neutral_reference_is_a_value() {
        let text = "providers:\n  verboo:\n    token: ${ARREO_ENV:VBK_PROD_KEY}\n";
        assert_eq!(
            references(text),
            vec!["VBK_PROD_KEY".to_string()],
            "the unquoted form is a reference the machine must resolve"
        );
        let out = denormalize(text, Dialect::OpencodeEnvBrace);
        assert!(
            out.contains("{env:VBK_PROD_KEY}"),
            "and it is rewritten on arrival: {out}"
        );
        // normalize() is the inverse: a native unquoted value becomes neutral.
        let native = "providers:\n  verboo:\n    token: VBK_PROD_KEY\n";
        let neutral = normalize(native);
        assert!(
            neutral.contains("${ARREO_ENV:VBK_PROD_KEY}"),
            "an omp bare name travels neutral: {neutral}"
        );
        assert_eq!(
            denormalize(&neutral, Dialect::OmpBareName),
            native,
            "and lands back as the bare name"
        );
    }
}

#[cfg(all(test, unix))]
mod private_mode_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// **A pre-existing store is made owner-only** (review F6). `OpenOptions::mode`
    /// is honoured only when the open creates the file, so a store that was ever
    /// group- or world-readable stayed that way while the warning claimed it had
    /// been rewritten. This is the file holding this machine's provider keys.
    #[test]
    fn setting_a_secret_repairs_a_readable_store() {
        let dir = std::env::temp_dir().join(format!(
            "arreo-keychain-mode-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("t").len()
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("secrets.json");
        std::fs::write(&path, b"{}").expect("seed");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        assert_eq!(
            std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777,
            0o644,
            "the test seeds a readable store"
        );

        let mut store = SecretStore::open(path.clone()).expect("open");
        store.set("VBK_PROD_KEY", "a-value").expect("set");
        assert_eq!(
            std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777,
            0o600,
            "and the write repairs it"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
