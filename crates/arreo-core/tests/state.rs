//! T-0004 failing-first probes: universal-tier state detection.
//!
//! Written before `src/state/` exists — MUST fail to compile until it lands.
//! Clock discipline: tests drive an explicit `now_ms` clock (no wall clock,
//! no sleeps) — deterministic, CI-stable, fast.

use arreo_core::state::{Adapter, Confidence, Engine, State};

fn fixture_path(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
}

fn load_raw(name: &str) -> Vec<u8> {
    arreo_core::fixtures::Fixture::load(&fixture_path(name))
        .expect("fixture loads")
        .replay_accelerated()
}

#[test]
fn streaming_output_is_working_then_idle_after_silence() {
    let adapter = Adapter::default();
    let mut engine = Engine::new(adapter, 0);
    // Stream the working fixture in 3 chunks: output flowing → working.
    let raw = load_raw("working-stream.pty");
    let third = raw.len() / 3;
    let mut t = 0u64;
    let mut saw_working = false;
    for chunk in raw.chunks(third.max(1)) {
        let events = engine.feed(chunk, t);
        if events.iter().any(|e| e.state == State::Working) {
            saw_working = true;
        }
        t += 50;
    }
    assert!(saw_working, "streaming output → working");
    // Silence past the idle threshold → idle.
    t += 5_000;
    let events = engine.feed(b"", t);
    assert!(
        events.iter().any(|e| e.state == State::Idle),
        "5 s silence → idle, got {events:?}"
    );
}

#[test]
fn permission_prompt_is_question_inferred_with_pattern() {
    let adapter = Adapter::default();
    let mut engine = Engine::new(adapter, 0);
    let raw = load_raw("question-permission.pty");
    let events = engine.feed(&raw, 100);
    // Output arrived recently; prompt shape at the tail → question.
    let later = engine.feed(b"", 100 + 2_500);
    let question = later
        .iter()
        .find(|e| e.state == State::Question)
        .expect("prompt-shaped silence → question, got {later:?}");
    assert!(
        matches!(question.confidence, Confidence::Inferred { .. }),
        "honestly inferred, got {:?}",
        question.confidence
    );
    assert!(
        question
            .matched_pattern
            .as_deref()
            .is_some_and(|p| !p.is_empty()),
        "matched pattern recorded"
    );
    let _ = events;
}

#[test]
fn child_exit_maps_to_done_with_code() {
    let adapter = Adapter::default();
    let mut engine = Engine::new(adapter, 0);
    let events = engine.child_exited(0, 500);
    assert!(events
        .iter()
        .any(|e| e.state == State::Done && e.exit_code == Some(0)));
    let mut engine = Engine::new(Adapter::default(), 0);
    let events = engine.child_exited(17, 500);
    // Non-zero exit is still done (not blocked — blocked is output-shape).
    assert!(events
        .iter()
        .any(|e| e.state == State::Done && e.exit_code == Some(17)));
}

#[test]
fn bell_means_attention_not_silence() {
    let adapter = Adapter::default();
    let mut engine = Engine::new(adapter, 0);
    engine.feed(b"working working working\n", 0);
    let events = engine.feed(b"\x07", 100);
    assert!(
        events
            .iter()
            .any(|e| e.state == State::Question || e.state == State::Blocked),
        "bell → attention (question or blocked), got {events:?}"
    );
}

#[test]
fn traceback_shape_is_blocked() {
    let adapter = Adapter::default();
    let mut engine = Engine::new(adapter, 0);
    let events = engine.feed(
        b"Traceback (most recent call last):\n  File \"x.py\", line 1\nValueError: bad\n",
        0,
    );
    // Error shape + subsequent silence → blocked.
    let later = engine.feed(b"", 3_000);
    assert!(
        later.iter().any(|e| e.state == State::Blocked),
        "traceback + silence → blocked, got {later:?}"
    );
    let _ = events;
}

#[test]
fn unknown_stays_unknown_without_evidence() {
    let adapter = Adapter::default();
    let engine = Engine::new(adapter, 0);
    assert_eq!(*engine.state(), State::Unknown);
}

#[test]
fn detection_latency_within_200ms_budget() {
    // Feed one byte, advance the clock in 1 ms steps: the state event for
    // the new output must appear with event.t_ms - byte.t_ms ≤ 200.
    let adapter = Adapter::default();
    let mut engine = Engine::new(adapter, 0);
    let events = engine.feed(b"x", 1_000);
    let first = events.first().expect("output produces an event");
    assert!(
        first.t_ms.saturating_sub(1_000) <= 200,
        "latency ≤ 200 ms, got {} ms",
        first.t_ms.saturating_sub(1_000)
    );
}

#[test]
fn default_adapter_loads_and_validates() {
    let adapter = Adapter::from_toml_file(
        &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../adapters/default.toml"),
    )
    .expect("default adapter loads");
    assert!(
        !adapter.question_patterns.is_empty(),
        "has question patterns"
    );
    assert!(adapter.idle_after_ms > 0, "has idle threshold");
}

#[test]
fn mid_line_question_marks_are_not_prompts() {
    // Adversarial (T-0017 preview): code full of "?" must not flip states.
    let mut engine = Engine::new(Adapter::default(), 0);
    engine.feed(b"fn f() {\n  // what? why?\n  let x = a ? b : c;\n}\n", 0);
    let later = engine.feed(b"", 3_000);
    assert!(
        !later.iter().any(|e| e.state == State::Question),
        "mid-line ? never asks, got {later:?}"
    );
}

#[test]
fn vim_screen_silence_is_not_a_question() {
    let mut engine = Engine::new(Adapter::default(), 0);
    engine.feed(b"\x1b[?1049h\x1b[1;1H~   vim   \x1b[24;1H\"file\" 1L", 0);
    let later = engine.feed(b"", 5_000);
    assert!(
        !later.iter().any(|e| e.state == State::Question),
        "editor screen never asks, got {later:?}"
    );
}

#[test]
fn question_fixture_timeline_is_working_then_question() {
    // Exact timeline: output at t=0 → Working(0); silence to t=2510 →
    // Question(2510, inferred, with pattern). Deterministic by construction.
    let raw = load_raw("question-permission.pty");
    let mut engine = Engine::new(Adapter::default(), 0);
    let first = engine.feed(&raw, 0);
    assert!(first
        .iter()
        .any(|e| e.state == State::Working && e.t_ms == 0));
    let later = engine.feed(b"", 2_510);
    let question = later
        .iter()
        .find(|e| e.state == State::Question)
        .expect("question after silence");
    assert_eq!(question.t_ms, 2_510);
    assert!(question.matched_pattern.as_deref().is_some());
}

#[test]
fn adapter_rejects_unknown_fields_and_bad_regex() {
    assert!(Adapter::from_toml(
        "idle_after_ms = 5\nquestion_patterns = ['a']\nerror_patterns = ['b']\nbogus_key = 1"
    )
    .is_err());
    assert!(Adapter::from_toml("question_patterns = ['([']\nerror_patterns = ['b']").is_err());
    assert!(Adapter::from_toml("question_patterns = []\nerror_patterns = ['b']").is_err());
}

#[test]
fn done_is_terminal_and_unknown_never_emits() {
    let mut engine = Engine::new(Adapter::default(), 0);
    engine.feed(b"output\n", 0);
    engine.child_exited(0, 100);
    assert!(engine.feed(b"late\n", 200).is_empty());
    assert!(engine.tick(999_999).is_empty());
    let mut fresh = Engine::new(Adapter::default(), 0);
    assert!(fresh.tick(999_999).is_empty());
    assert_eq!(*fresh.state(), State::Unknown);
}
