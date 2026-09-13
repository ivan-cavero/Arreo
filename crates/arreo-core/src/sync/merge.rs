//! Merging two versions of a config file — and the three ways that goes wrong
//! (T-0083).
//!
//! One sentence: keep-both is the default (the loser is written as
//! `<name>.conflict-<machine>-<ts>.<ext>` beside the winner), an explicit merge
//! reconciles two comment-free JSON documents, and every way the merge could
//! silently lose or invent something is a refusal with a reason instead.
//!
//! **Why keep-both rather than newest-wins.** ROADMAP §3.8 says so, and the
//! reason is visible in the owner's file: a provider block is *intent*. Two
//! machines that each added a provider have both done something worth keeping,
//! and any automatic pick throws one away; a three-way merge is the real answer
//! and it is not what this ticket ships, so the honest behaviour is to keep both
//! files and say so. The losing copy carries the losing machine's name and the
//! instant, because "which one is which" is the first thing the operator asks.
//!
//! ## The three hazards T-0075 verified, handled here
//!
//! 1. **A sibling extension merges too.** With `opencode.json` *and*
//!    `opencode.jsonc` both present, opencode lists the providers of both, so
//!    writing the `.jsonc` while the `.json` exists puts two provider lists live
//!    at once. [`crate::sync::engine`] refuses to write in that case, and
//!    [`MergeSpec`]-driven reconciliation folds the sibling in and renames it
//!    aside.
//! 2. **JSONC comments.** `//` and `/* */` are legal in the real file format,
//!    and this merge re-emits what it parsed. Persisting the operator's comments
//!    through that round trip would need a comment-preserving parser (a new
//!    dependency, and the task's constraint is explicit that this is a stop
//!    condition rather than an install), so a document that carries comments is
//!    **refused** with a reason. Reformatting a commented file — dropping the
//!    comments and saying nothing — is the one outcome worse than refusing.
//! 3. **Array keys are sets.** `plugin` is a list of installed plugins: the
//!    union is the merge. Replacing it would uninstall the peer's plugins, which
//!    is a silent behaviour change on a machine nobody is looking at.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

/// Which of two documents is being described, for a refusal that has to say
/// which side carried the problem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The copy already on this machine.
    Live,
    /// The copy arriving from the peer.
    Incoming,
}

impl Side {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Side::Live => "this machine's copy",
            Side::Incoming => "the incoming copy",
        }
    }
}

/// Why a merge did not happen. Every variant is a refusal, never a silent
/// choice — the operator is told which key, which side, and what to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeRefusal {
    /// The document has JSONC comments this merge would not preserve.
    Comments { side: Side },
    /// Both documents set the same key to different values.
    Conflict { path: String },
    /// Both documents use the same key for different kinds of value.
    Shape { path: String },
    /// The document is not JSON this merge can read.
    Parse { side: Side, reason: String },
}

impl std::fmt::Display for MergeRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MergeRefusal::Comments { side } => write!(
                f,
                "{} carries JSONC comments, and this merge re-emits the document it parsed — \
                 it would silently drop them; merge by hand, or revert",
                side.as_str()
            ),
            MergeRefusal::Conflict { path } => write!(
                f,
                "both copies set {path} to different values; nothing was written — decide by \
                 hand (the copy that does not win is kept beside the file)"
            ),
            MergeRefusal::Shape { path } => write!(
                f,
                "both copies use {path} for different kinds of value (object, array, scalar); \
                 nothing was written"
            ),
            MergeRefusal::Parse { side, reason } => {
                write!(f, "{} is not JSON: {reason}", side.as_str())
            }
        }
    }
}

impl std::error::Error for MergeRefusal {}

/// What a merge needs to know about the file it is merging.
///
/// The array-union list comes from the preset (`opencode`'s `plugin` is the one
/// T-0075 verified), so the rule lives with the data rather than in the merge.
#[derive(Debug, Clone, Copy)]
pub struct MergeSpec {
    pub union_arrays: &'static [&'static str],
}

impl MergeSpec {
    #[must_use]
    pub fn new(union_arrays: &'static [&'static str]) -> Self {
        Self { union_arrays }
    }
}

/// `text` with every JSONC comment removed, strings untouched.
///
/// The companion of [`has_comments`] and written the same way — a scanner, not a
/// search — because the same `"https://…"` trap applies to removal: cutting at
/// the first `//` inside a string would corrupt the very config the caller is
/// about to validate. Used by the receiver's format check (T-0083 F5), which
/// needs to know whether a `.jsonc` payload is at least a parseable document.
#[must_use]
pub fn strip_jsonc_comments(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => {
                let start = index;
                index += 1;
                while index < bytes.len() {
                    match bytes[index] {
                        b'\\' => index += 2,
                        b'"' => break,
                        _ => index += 1,
                    }
                }
                index = (index + 1).min(bytes.len());
                out.push_str(&text[start..index]);
            }
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index += 2;
                while index < bytes.len()
                    && !(bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/'))
                {
                    index += 1;
                }
                index = (index + 2).min(bytes.len());
            }
            _ => {
                out.push(bytes[index] as char);
                index += 1;
            }
        }
    }
    out
}

/// Does `text` carry a JSONC comment outside a string literal?
///
/// A scanner rather than a search for `//`, because `"https://…"` is the common
/// case in exactly these files: a base URL is a value, not a comment.
#[must_use]
pub fn has_comments(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => {
                index += 1;
                while index < bytes.len() {
                    match bytes[index] {
                        b'\\' => index += 2,
                        b'"' => break,
                        _ => index += 1,
                    }
                }
                index += 1;
            }
            b'/' if index + 1 < bytes.len()
                && (bytes[index + 1] == b'/' || bytes[index + 1] == b'*') =>
            {
                return true;
            }
            _ => index += 1,
        }
    }
    false
}

/// Merge `incoming` into `base`, or refuse and say why.
///
/// The result is pretty-printed JSON with a trailing newline. Key order is the
/// parser's (alphabetical), which is a visible reformat of a hand-written file —
/// stated here rather than discovered by an operator whose provider list
/// reordered itself: this function is only called when someone asked for a
/// merge, and a commented file (where the reformat would also lose prose) is
/// refused above.
pub fn merge_json(base: &str, incoming: &str, spec: &MergeSpec) -> Result<String, MergeRefusal> {
    if has_comments(base) {
        return Err(MergeRefusal::Comments { side: Side::Live });
    }
    if has_comments(incoming) {
        return Err(MergeRefusal::Comments {
            side: Side::Incoming,
        });
    }
    let base: Value = serde_json::from_str(base).map_err(|e| MergeRefusal::Parse {
        side: Side::Live,
        reason: e.to_string(),
    })?;
    let incoming: Value = serde_json::from_str(incoming).map_err(|e| MergeRefusal::Parse {
        side: Side::Incoming,
        reason: e.to_string(),
    })?;
    let merged = merge_value("", &base, &incoming, spec)?;
    let mut text = serde_json::to_string_pretty(&merged).map_err(|e| MergeRefusal::Parse {
        side: Side::Incoming,
        reason: e.to_string(),
    })?;
    text.push('\n');
    Ok(text)
}

fn merge_value(
    path: &str,
    base: &Value,
    incoming: &Value,
    spec: &MergeSpec,
) -> Result<Value, MergeRefusal> {
    match (base, incoming) {
        (Value::Object(base_map), Value::Object(incoming_map)) => {
            let mut merged: Map<String, Value> = base_map.clone();
            for (key, incoming_value) in incoming_map {
                let child = child_path(path, key);
                match merged.get(key) {
                    None => {
                        merged.insert(key.clone(), incoming_value.clone());
                    }
                    Some(base_value) => {
                        let value = merge_value(&child, base_value, incoming_value, spec)?;
                        merged.insert(key.clone(), value);
                    }
                }
            }
            Ok(Value::Object(merged))
        }
        (Value::Array(base_items), Value::Array(incoming_items)) => {
            // The union is only sound for keys the preset declares as sets. For
            // any other array, "both sides changed it" is a conflict the
            // operator decides, because a positional list (a model list with
            // ordering) has no set semantics to union.
            if spec.union_arrays.contains(&last_key(path).unwrap_or("")) {
                let mut union = base_items.clone();
                for item in incoming_items {
                    if !union.contains(item) {
                        union.push(item.clone());
                    }
                }
                Ok(Value::Array(union))
            } else if base_items == incoming_items {
                Ok(Value::Array(base_items.clone()))
            } else {
                Err(MergeRefusal::Conflict {
                    path: path.to_string(),
                })
            }
        }
        (Value::Null, other) | (other, Value::Null) => Ok(other.clone()),
        _ if base == incoming => Ok(base.clone()),
        _ if same_kind(base, incoming) => Err(MergeRefusal::Conflict {
            path: path.to_string(),
        }),
        _ => Err(MergeRefusal::Shape {
            path: path.to_string(),
        }),
    }
}

fn same_kind(a: &Value, b: &Value) -> bool {
    matches!(
        (a, b),
        (Value::Bool(_), Value::Bool(_))
            | (Value::Number(_), Value::Number(_))
            | (Value::String(_), Value::String(_))
            | (Value::Object(_), Value::Object(_))
            | (Value::Array(_), Value::Array(_))
    )
}

fn child_path(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{path}.{key}")
    }
}

fn last_key(path: &str) -> Option<&str> {
    path.rsplit('.').next()
}

/// The path a losing copy is written to: `<stem>.conflict-<machine>-<ts>.<ext>`.
///
/// The machine is the one whose content lost, and the stamp is UTC — so the
/// name answers "whose edit is this and when did we notice" without opening it,
/// and two conflicts a second apart cannot collide.
#[must_use]
pub fn conflict_path(dest: &Path, machine: &str, ms: u64) -> PathBuf {
    let stem = dest
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("config");
    let machine = sanitize_machine(machine);
    let name = match dest.extension().and_then(|e| e.to_str()) {
        Some(extension) => format!(
            "{stem}.conflict-{machine}-{}.{extension}",
            compact_utc_ms(ms)
        ),
        None => format!("{stem}.conflict-{machine}-{}", compact_utc_ms(ms)),
    };
    dest.with_file_name(name)
}

/// Is `candidate` a conflict copy of `dest`?
#[must_use]
pub fn is_conflict_copy(dest: &Path, candidate: &Path) -> bool {
    let Some(stem) = dest.file_stem().and_then(|s| s.to_str()) else {
        return false;
    };
    let Some(name) = candidate.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if !name.starts_with(&format!("{stem}.conflict-")) {
        return false;
    }
    match dest.extension().and_then(|e| e.to_str()) {
        Some(extension) => name.ends_with(&format!(".{extension}")),
        None => true,
    }
}

/// A machine name reduced to what belongs in a file name.
fn sanitize_machine(machine: &str) -> String {
    let cleaned: String = machine
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "machine".to_string()
    } else {
        cleaned
    }
}

/// An epoch-millisecond instant as `YYYYMMDDTHHMMSS` (UTC), for a file name.
///
/// The compact form exists because the name has to sort chronologically as text
/// and survive every filesystem. The arithmetic is Hinnant's days-to-civil, the
/// same algorithm [`crate::store::rfc3339_ms`] uses for the human form; the two
/// are cross-checked by a test so a fix to one cannot drift from the other.
///
/// The input is `u64` because its only caller is "now" for a file name; the
/// day count that reaches [`civil_from_days`] is `u64::MAX / 86_400_000`, which
/// is four orders of magnitude inside `i64`, so the conversion is exact for
/// every value this function can be given.
#[must_use]
pub fn compact_utc_ms(ms: u64) -> String {
    let seconds = ms / 1000;
    let days = (seconds / 86_400) as i64;
    let rem = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Days since 1970-01-01 to a civil date, proleptic Gregorian (Hinnant).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPENCODE: MergeSpec = MergeSpec {
        union_arrays: &["plugin"],
    };

    #[test]
    fn two_provider_lists_merge_by_union() {
        let live = r#"{"provider":{"verboo":{"name":"Verboo"}}}"#;
        let incoming = r#"{"provider":{"other":{"name":"Other"}}}"#;
        let merged = merge_json(live, incoming, &OPENCODE).expect("merge");
        let value: Value = serde_json::from_str(&merged).expect("json");
        assert!(value["provider"]["verboo"].is_object());
        assert!(value["provider"]["other"].is_object());
    }

    #[test]
    fn the_same_provider_id_defined_twice_is_a_conflict_not_a_winner() {
        let live = r#"{"provider":{"verboo":{"name":"Mine","npm":"a"}}}"#;
        let incoming = r#"{"provider":{"verboo":{"name":"Theirs","npm":"a"}}}"#;
        let refusal = merge_json(live, incoming, &OPENCODE).expect_err("must refuse");
        assert_eq!(
            refusal,
            MergeRefusal::Conflict {
                path: "provider.verboo.name".to_string()
            }
        );
        assert!(refusal.to_string().contains("nothing was written"));
    }

    #[test]
    fn a_plugin_array_is_a_set_and_unions_without_duplicates() {
        let live = r#"{"plugin":["a.js","b.js"]}"#;
        let incoming = r#"{"plugin":["b.js","c.js"]}"#;
        let merged = merge_json(live, incoming, &OPENCODE).expect("merge");
        let value: Value = serde_json::from_str(&merged).expect("json");
        assert_eq!(value["plugin"], serde_json::json!(["a.js", "b.js", "c.js"]));
    }

    #[test]
    fn an_undeclared_array_is_a_conflict_instead_of_a_union() {
        // A model list has order and identity; unioning it would invent a list
        // neither machine wrote.
        let live = r#"{"providers":{"p":{"models":["m1"]}}}"#;
        let incoming = r#"{"providers":{"p":{"models":["m2"]}}}"#;
        let refusal = merge_json(live, incoming, &OPENCODE).expect_err("must refuse");
        assert_eq!(
            refusal,
            MergeRefusal::Conflict {
                path: "providers.p.models".to_string()
            }
        );
    }

    #[test]
    fn jsonc_comments_refuse_the_merge_rather_than_getting_dropped() {
        let commented = "{\n  // the owner's note\n  \"provider\": {}\n}";
        let refusal = merge_json(commented, "{}", &OPENCODE).expect_err("must refuse");
        assert_eq!(refusal, MergeRefusal::Comments { side: Side::Live });
        assert!(refusal.to_string().contains("drop"));
        // And the detector is not fooled by a URL, which is why it exists.
        assert!(!has_comments(
            r#"{"url":"https://code.verboo.ai/router/v1"}"#
        ));
        assert!(!has_comments(r#"{"a":"/* not a comment */"}"#));
        assert!(has_comments("{ /* yes */ }"));
    }

    #[test]
    fn a_conflict_copy_is_named_for_the_machine_that_lost_and_sorts_by_time() {
        let dest = Path::new("/cfg/opencode/opencode.jsonc");
        // The instant from `docs/harness-centralization.md` §4 step 6, so the
        // name in the docs and the name the code produces are the same name.
        let first = conflict_path(dest, "mac", 1_789_294_500_000);
        assert_eq!(
            first,
            PathBuf::from("/cfg/opencode/opencode.conflict-mac-20260913T101500.jsonc")
        );
        let second = conflict_path(dest, "mac", 1_789_294_501_000);
        assert!(second > first, "later conflicts sort later");
        // A machine name with a slash cannot escape the directory.
        let odd = conflict_path(dest, "a/b", 1_789_294_500_000);
        assert_eq!(
            odd.file_name().and_then(|n| n.to_str()),
            Some("opencode.conflict-a-b-20260913T101500.jsonc")
        );
        assert!(is_conflict_copy(dest, &first));
        assert!(!is_conflict_copy(
            dest,
            Path::new("/cfg/opencode/opencode.jsonc")
        ));
    }

    /// The compact stamp and the store's RFC3339 rendering are two spellings of
    /// one instant; this pins them to each other so a fix in one cannot drift
    /// from the other. Gated on the store's feature because the pairing is the
    /// point — outside `sqlite` there is no second spelling to agree with.
    #[cfg(feature = "sqlite")]
    #[test]
    fn the_stamp_agrees_with_the_stores_human_form() {
        for ms in [
            0_u64,
            1_789_294_500_000,
            1_789_294_501_000,
            2_000_000_000_000,
        ] {
            let compact = compact_utc_ms(ms);
            let human: String = crate::store::rfc3339_ms(ms as i64)
                .trim_end_matches('Z')
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .collect();
            assert_eq!(compact, human, "at {ms}");
        }
    }
}

#[cfg(test)]
mod comment_strip_tests {
    use super::strip_jsonc_comments;

    /// The `"https://…"` trap, from the removal side: a base URL is a value, and
    /// cutting at the first `//` would corrupt the document the receiver is
    /// about to validate (T-0083 F5).
    #[test]
    fn stripping_comments_leaves_strings_alone() {
        let text = "{\n  // a note\n  \"baseURL\": \"https://code.verboo.ai/v1\", /* inline */\n  \"n\": 1\n}\n";
        let stripped = strip_jsonc_comments(text);
        assert!(
            stripped.contains("https://code.verboo.ai/v1"),
            "the URL survived: {stripped}"
        );
        assert!(
            !stripped.contains("a note"),
            "the comment is gone: {stripped}"
        );
        assert!(
            !stripped.contains("inline"),
            "both comment kinds: {stripped}"
        );
        let value: serde_json::Value = serde_json::from_str(&stripped).expect("parses");
        assert_eq!(value["n"], serde_json::json!(1));
    }
}
