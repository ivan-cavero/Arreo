//! T-0011 failing-first probes: fixture record → save → load → replay.
//!
//! Written before `src/fixtures.rs` exists — MUST fail to compile until it lands.

use arreo_core::fixtures::{scan_secrets, Fixture};
use std::time::Duration;

#[test]
fn record_replay_round_trip_is_byte_exact() {
    let rec = Fixture::record(
        &["/bin/echo", "round-trip-bytes-99"],
        Duration::from_secs(10),
    )
    .expect("record echo");
    assert!(
        rec.text().contains("round-trip-bytes-99"),
        "capture holds the output"
    );
    let dir = std::env::temp_dir().join(format!("arreo-fixt-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("roundtrip.pty");
    rec.save(&path).expect("save");
    let loaded = Fixture::load(&path).expect("load");
    assert_eq!(
        loaded.events.len(),
        rec.events.len(),
        "event count survives"
    );
    // Accelerated replay feeds the same bytes, pacing collapsed.
    let replayed = loaded.replay_accelerated();
    let orig: Vec<u8> = rec.events.iter().flat_map(|e| e.raw_bytes()).collect();
    assert_eq!(replayed, orig, "replay is byte-exact");
}

#[test]
fn secret_scan_flags_keys_before_commit() {
    let flagged = scan_secrets("export OPENAI_API_KEY=sk-abc123XYZ4567890abcdef\n");
    assert!(!flagged.is_empty(), "api-key-shaped content flagged");
    let clean = scan_secrets("echo hello world\n");
    assert!(clean.is_empty(), "benign output passes");
}

/// **A reference is not a secret** (T-0083). §3.8 tells the operator to write
/// `{env:NAME}` instead of a key, and the scan used to refuse exactly that — it
/// fired on the field name and never looked at the value — while letting a real
/// key through under a different field name. Both halves are pinned here, in
/// every dialect T-0075 measured: opencode `{env:NAME}`, pi `$NAME` and
/// `${NAME}`, omp the bare variable name.
#[test]
fn a_reference_is_not_a_secret_in_any_dialect() {
    for line in [
        r#""apiKey": "{env:VBK_PROD_KEY}""#,
        r#"apiKey: $VBK_PROD_KEY"#,
        r#"apiKey: ${VBK_PROD_KEY}"#,
        r#"apiKey: VBK_PROD_KEY"#,
        r#"OPENAI_API_KEY=${OPENAI_API_KEY}"#,
        r#"client_secret = $MY_CLIENT_SECRET"#,
    ] {
        assert!(
            scan_secrets(line).is_empty(),
            "a reference must not be flagged: {line} -> {:?}",
            scan_secrets(line)
        );
    }
}

/// The other half of the same rule: the *same field* holding a literal is still
/// a secret. A rule that made `apiKey` exempt would trade a false positive for a
/// false negative — the exact failure the scan exists to prevent.
#[test]
fn a_literal_in_the_same_field_is_still_flagged() {
    for line in [
        r#""apiKey": "vbk_pro_0123456789abcdef0123456789abcdef01234567""#,
        r#"apiKey: sk-0123456789abcdefghijklmnop"#,
        r#"apiKey: AKIAIOSFODNN7EXAMPLE"#,
        r#"OPENAI_API_KEY=sk-abc123XYZ4567890abcdef"#,
    ] {
        assert!(
            !scan_secrets(line).is_empty(),
            "a literal must still be flagged: {line}"
        );
    }
}

/// The provider shapes T-0075 measured, plus the JWT the survey's own probe
/// used. Each is a literal that appears nowhere else, so a real key in the same
/// shape still fires — the point of a prefix rule rather than a value allowlist.
#[test]
fn the_measured_provider_shapes_are_flagged() {
    let shapes = [
        "vbk_pro_0123456789abcdef0123456789abcdef01234567",
        "xai-0123456789abcdefghijklmnopqrstuvwx",
        "glpat-0123456789abcdefghij",
        "hf_0123456789abcdefghijklmnop",
        "AIzaSyA0123456789abcdefghijklmnopqrstuv",
        "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U",
    ];
    for shape in shapes {
        // Bare, and behind a field name that carries no hint at all: the token
        // rule is value-shaped, so the field must not decide it.
        assert!(!scan_secrets(shape).is_empty(), "a bare {shape} is a token");
        assert!(
            !scan_secrets(&format!("token: {shape}")).is_empty(),
            "and stays one behind an innocent field name: {shape}"
        );
    }
}

/// A reference-shaped *value* is not a licence to hide a token beside it: the
/// token rules run on the value, not on the field.
#[test]
fn a_reference_shaped_field_does_not_hide_a_token() {
    let findings =
        scan_secrets(r#"apiKey: {env:VBK_PROD_KEY} # was sk-0123456789abcdefghijklmnop"#);
    assert!(
        findings.iter().any(|f| f.contains("sk-")),
        "the literal beside the reference is still found: {findings:?}"
    );
}

/// A short run after a prefix is not a token — `ask-me` contains `sk-`. Pinned
/// because the new prefixes must not lower the bar: `hf_` and `vbk_` are
/// ordinary word fragments in prose.
#[test]
fn short_runs_after_the_new_prefixes_are_not_tokens() {
    for line in [
        "the vbk_ prefix and the hf_ prefix are documented here",
        "xai- is three letters",
        "AIza is the start of a google key",
    ] {
        assert!(
            scan_secrets(line).is_empty(),
            "prose mentioning a prefix is not a secret: {line} -> {:?}",
            scan_secrets(line)
        );
    }
}

#[test]
fn empty_session_records_and_replays_cleanly() {
    let rec = Fixture::record(&["/bin/true"], Duration::from_secs(10)).expect("record true");
    let bytes: Vec<u8> = rec.events.iter().flat_map(|e| e.raw_bytes()).collect();
    let _ = bytes;
    let dir = std::env::temp_dir().join(format!("arreo-fixt-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("empty.pty");
    rec.save(&path).expect("save");
    let loaded = Fixture::load(&path).expect("load");
    assert_eq!(loaded.replay_accelerated(), Vec::<u8>::new());
}

/// Committed fixtures stay loadable, byte-stable, and secret-free.
/// This is the CI gate for the T-0011 acceptance criteria.
#[test]
fn committed_fixtures_are_stable_and_clean() {
    use arreo_core::fixtures::{scan_secrets, Fixture};
    use std::path::PathBuf;

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dir = root.join("../../fixtures");
    let expected = [
        "question-permission.pty",
        "working-stream.pty",
        "idle-shell.pty",
        "vim-edit.pty",
        "locale-utf8.pty",
    ];
    for name in expected {
        let path = dir.join(name);
        assert!(path.exists(), "fixture committed: {name}");
        let fixture = Fixture::load(&path).expect("fixture loads");
        assert!(!fixture.events.is_empty(), "{name} holds events");
        // Deterministic replay: two loads, same bytes.
        let again = Fixture::load(&path).expect("fixture reloads");
        assert_eq!(
            fixture.replay_accelerated(),
            again.replay_accelerated(),
            "{name} replays deterministically"
        );
        // Small: every committed fixture < 64 KiB.
        let size = std::fs::metadata(&path).expect("stat").len();
        assert!(size < 64 * 1024, "{name} is small ({size} bytes)");
        // Clean: no secret-shaped content.
        let findings = scan_secrets(&fixture.text());
        assert!(findings.is_empty(), "{name} secret-free, got {findings:?}");
    }
    // Spot content: the question fixture really asks, the vim one really
    // drove the alternate screen, the locale one kept its multibyte text.
    let q = Fixture::load(&dir.join("question-permission.pty")).unwrap();
    assert!(q.text().contains("[y/n]"), "question fixture asks");
    let vim = Fixture::load(&dir.join("vim-edit.pty")).unwrap();
    assert!(
        vim.replay_accelerated().contains(&0x1b),
        "vim fixture holds real escapes"
    );
    let loc = Fixture::load(&dir.join("locale-utf8.pty")).unwrap();
    assert!(
        loc.text().contains("日本語"),
        "locale fixture keeps multibyte"
    );
}
