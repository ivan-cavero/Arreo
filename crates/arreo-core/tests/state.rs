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

// ————— T-0080: an OSC BEL is not a bell —————

/// T-0080 product repro: a pane whose whole output is one OSC 8 hyperlink
/// must NOT report blocked. `ESC ]8;;…` opens the link; the BEL terminates
/// the OSC string — it is not a ring-the-bell byte.
#[test]
fn osc_hyperlink_bel_is_not_a_bell() {
    let adapter = Adapter::default();
    let mut engine = Engine::new(adapter, 0);
    let bytes = b"\x1b]8;;http://example.com\x07click\x1b]8;;\x07";
    let flow = engine.feed(bytes, 0);
    assert!(
        !flow
            .iter()
            .any(|e| e.state == State::Question || e.state == State::Blocked),
        "OSC-terminated BELs are not attention, got {flow:?}"
    );
    assert!(
        flow.iter().any(|e| e.state == State::Working),
        "the link still rendered (working), got {flow:?}"
    );
    let later = engine.feed(b"", 5_000);
    assert!(
        !later
            .iter()
            .any(|e| e.state == State::Question || e.state == State::Blocked),
        "silence after a hyperlink must not ask, got {later:?}"
    );
    assert_eq!(*engine.state(), State::Idle);
}

/// Every OSC terminator (BEL, ST = `ESC \`, C1 `0x9c`) closes the string
/// without ringing a bell — and a real bare BEL after the string still means
/// attention exactly as before the escape-aware change. This is the
/// both-halves test: no false positive traded for a false negative.
#[test]
fn osc_terminators_are_not_bells_but_bare_bel_still_is() {
    let adapter = Adapter::default();
    let osc_only: &[&[u8]] = &[
        // Window title, BEL-terminated, then plain text.
        b"\x1b]0;build\x07ready",
        // opencode's iTerm2 capability probe, ST-terminated.
        b"\x1b]1337;Capabilities\x1b\\done",
        // Colour query, C1-ST-terminated.
        b"\x1b]11;?\x9cdone",
        // OSC 8 hyperlink open + close.
        b"\x1b]8;;http://example.com\x07click\x1b]8;;\x07",
    ];
    for case in osc_only {
        let mut engine = Engine::new(adapter.clone(), 0);
        engine.feed(b"working\n", 0);
        let flow = engine.feed(case, 100);
        let later = engine.feed(b"", 5_000);
        assert!(
            !flow
                .iter()
                .chain(later.iter())
                .any(|e| e.state == State::Question || e.state == State::Blocked),
            "OSC terminators alone never mean attention: {case:?} → {flow:?} {later:?}"
        );
        assert_eq!(
            *engine.state(),
            State::Idle,
            "OSC terminated string + silence → idle: {case:?}"
        );
    }

    let osc_then_bare_bel: &[&[u8]] = &[
        // Title (BEL-terminated), then a REAL bell.
        b"\x1b]0;build\x07ready\x07",
        // Capability probe (ST-terminated), then a REAL bell.
        b"\x1b]1337;Capabilities\x1b\\done\x07",
        // Colour query (C1-ST-terminated), then a REAL bell.
        b"\x1b]11;?\x9cdone\x07",
    ];
    for case in osc_then_bare_bel {
        let mut engine = Engine::new(adapter.clone(), 0);
        engine.feed(b"working\n", 0);
        let flow = engine.feed(case, 100);
        assert!(
            flow.iter()
                .any(|e| e.state == State::Question || e.state == State::Blocked),
            "the bare BEL after the OSC string is attention: {case:?} → {flow:?}"
        );
    }
}

/// The escape stream state must survive feed boundaries: the daemon polls
/// arbitrary chunks, so an OSC string (or the ESC that opens it) can straddle
/// two feeds. A BEL one feed later must still be read as that OSC's
/// terminator, never a bell.
#[test]
fn osc_state_survives_feed_boundaries() {
    let adapter = Adapter::default();

    // OSC string split mid-content.
    let mut engine = Engine::new(adapter.clone(), 0);
    engine.feed(b"\x1b]8;;http://exa", 0);
    let flow = engine.feed(b"mple.com\x07click\x1b]8;;\x07", 100);
    assert!(
        !flow
            .iter()
            .any(|e| e.state == State::Question || e.state == State::Blocked),
        "a BEL terminating an OSC opened in an earlier feed is not a bell: {flow:?}"
    );

    // Feed ending exactly at the ESC of the OSC opener.
    let mut engine = Engine::new(adapter.clone(), 0);
    engine.feed(b"work\x1b", 0);
    let flow = engine.feed(b"]8;;http://example.com\x07", 100);
    assert!(
        !flow
            .iter()
            .any(|e| e.state == State::Question || e.state == State::Blocked),
        "ESC split across feeds still opens the OSC: {flow:?}"
    );

    // Feed ending exactly at the ESC of an ST inside an OSC.
    let mut engine = Engine::new(adapter.clone(), 0);
    engine.feed(b"\x1b]1337;Capabilities\x1b", 0);
    let flow = engine.feed(b"\\", 100);
    assert!(
        !flow
            .iter()
            .any(|e| e.state == State::Question || e.state == State::Blocked),
        "ST split across feeds still terminates the OSC: {flow:?}"
    );

    // And the next feed's BEL is back to being a real bell: the state did
    // not get stuck inside the closed OSC.
    for case in [&b"\x1b]0;title\x1b"[..], &b"\x1b]1337;Capabilities\x1b"[..]] {
        let mut engine = Engine::new(adapter.clone(), 0);
        engine.feed(b"working\n", 0);
        engine.feed(case, 100);
        let flow = engine.feed(b"\\\x07", 200);
        assert!(
            flow.iter()
                .any(|e| e.state == State::Question || e.state == State::Blocked),
            "after the ST closes the OSC, the next BEL is a real bell: {flow:?}"
        );
    }
}

/// A BEL inside a CSI sequence (not an OSC string) is untouched by the
/// escape-aware narrowing: it still means attention today and must tomorrow.
#[test]
fn csi_embedded_bel_is_still_a_bell() {
    let adapter = Adapter::default();
    let mut engine = Engine::new(adapter, 0);
    engine.feed(b"working\n", 0);
    // Malformed CSI that smuggles a BEL mid-sequence — not an OSC string, so
    // the BEL is a real one (strict narrowing: only OSC content changed).
    let flow = engine.feed(b"\x1b[2;7m\x07", 100);
    assert!(
        flow.iter()
            .any(|e| e.state == State::Question || e.state == State::Blocked),
        "a BEL outside any OSC string means attention: {flow:?}"
    );
}

/// T-0080: a harness adapter declared in `adapters/*.toml` must be *reachable*
/// by the running daemon. The registry is compiled in ([`AdapterRegistry::
/// builtin`]), so a new TOML that nobody added there is a file the tests can
/// read and the daemon cannot: panes of that program silently fall through to
/// `default.toml`. This is the criterion-4 failure mode for omp, pinned.
#[test]
fn every_declared_harness_adapter_is_in_the_builtin_registry() {
    use arreo_core::state::AdapterRegistry;
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../adapters");
    let registry = AdapterRegistry::builtin();
    let mut harnesses = 0usize;
    for entry in std::fs::read_dir(&root).expect("adapters dir") {
        let path = entry.expect("dirent").path();
        if path.extension().is_none_or(|e| e != "toml") {
            continue;
        }
        let adapter = Adapter::load(&path).expect("adapter loads");
        let Some(harness) = adapter.harness_id() else {
            continue; // the universal adapter owns everything unclaimed
        };
        harnesses += 1;
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        assert_eq!(
            registry.by_harness(harness).map(|a| a.harness_id()),
            Some(Some(harness)),
            "{name}: harness {harness:?} is unknown to the builtin registry"
        );
        for program in &adapter.programs {
            assert_eq!(
                registry.for_program(program).harness_id(),
                Some(harness),
                "{name}: program {program:?} does not select harness {harness:?}"
            );
        }
    }
    assert!(harnesses >= 3, "pi, opencode and omp are all declared");
    // And the program the T-0080 pane runs is owned by omp, not the default.
    assert_eq!(
        AdapterRegistry::builtin()
            .for_program("/home/me/.bun/bin/omp")
            .harness_id(),
        Some("omp"),
        "a path to omp still matches by basename"
    );
    assert_eq!(
        AdapterRegistry::builtin()
            .for_program("some-unclaimed-tool")
            .harness_id(),
        None,
        "everything else stays on the universal adapter"
    );
}

/// T-0080 recording-based regression: the pi TUI capture (full of OSC 8
/// hyperlinks and an OSC 0 title — every BEL an OSC terminator, 60/60) must
/// replay to a non-attention state.
#[test]
fn pi_tui_capture_replay_is_not_blocked() {
    let adapter = Adapter::default();
    let mut engine = Engine::new(adapter, 0);
    let raw = load_raw("pi-tui.pty");
    let mut t = 0u64;
    for chunk in raw.chunks(512) {
        let events = engine.feed(chunk, t);
        assert!(
            !events
                .iter()
                .any(|e| e.state == State::Question || e.state == State::Blocked),
            "pi TUI bells are OSC terminators, not attention: {events:?}"
        );
        t += 50;
    }
    let later = engine.feed(b"", t + 5_000);
    assert!(
        !later
            .iter()
            .any(|e| e.state == State::Question || e.state == State::Blocked),
        "pi TUI + silence must not ask or block: {later:?}"
    );
    assert_ne!(*engine.state(), State::Blocked);
}

/// T-0080 recording-based regression: the opencode TUI capture (title OSC,
/// colour queries, the iTerm2/kitty probes — every BEL an OSC terminator,
/// 5/5) must replay to a non-attention state.
#[test]
fn opencode_tui_capture_replay_is_not_blocked() {
    let adapter = Adapter::default();
    let mut engine = Engine::new(adapter, 0);
    let raw = load_raw("opencode-tui.pty");
    let mut t = 0u64;
    for chunk in raw.chunks(512) {
        let events = engine.feed(chunk, t);
        assert!(
            !events
                .iter()
                .any(|e| e.state == State::Question || e.state == State::Blocked),
            "opencode TUI bells are OSC terminators, not attention: {events:?}"
        );
        t += 50;
    }
    let later = engine.feed(b"", t + 5_000);
    assert!(
        !later
            .iter()
            .any(|e| e.state == State::Question || e.state == State::Blocked),
        "opencode TUI + silence must not ask or block: {later:?}"
    );
    assert_ne!(*engine.state(), State::Blocked);
}

/// T-0080 synthetic halves, as fixtures: the bare-BEL fixture must produce
/// attention when replayed; the OSC-terminator fixture must never.
#[test]
fn bare_bel_fixture_means_attention() {
    let adapter = Adapter::default();
    let mut engine = Engine::new(adapter, 0);
    let raw = load_raw("bell-bare.pty");
    let mut t = 0u64;
    let mut saw_attention = false;
    for chunk in raw.chunks(512) {
        let events = engine.feed(chunk, t);
        if events
            .iter()
            .any(|e| e.state == State::Question || e.state == State::Blocked)
        {
            saw_attention = true;
        }
        t += 50;
    }
    assert!(
        saw_attention,
        "the bare BEL fixture must mean attention on replay"
    );
}

#[test]
fn osc_terminator_fixture_means_no_attention() {
    let adapter = Adapter::default();
    let mut engine = Engine::new(adapter, 0);
    let raw = load_raw("osc-terminator.pty");
    let mut t = 0u64;
    for chunk in raw.chunks(512) {
        let events = engine.feed(chunk, t);
        assert!(
            !events
                .iter()
                .any(|e| e.state == State::Question || e.state == State::Blocked),
            "OSC terminators never mean attention on replay: {events:?}"
        );
        t += 50;
    }
    let later = engine.feed(b"", t + 5_000);
    assert!(
        !later
            .iter()
            .any(|e| e.state == State::Question || e.state == State::Blocked),
        "OSC-terminator fixture + silence must not ask or block: {later:?}"
    );
    assert_eq!(*engine.state(), State::Idle);
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

#[test]
fn multibyte_truncation_never_panics() {
    // Chaos-found (T-0009): TEXT_CAP drain split a multibyte char.
    let mut engine = Engine::new(Adapter::default(), 0);
    let chunk = "日本語✓".repeat(20000).into_bytes();
    for (i, piece) in chunk.chunks(1024).enumerate() {
        engine.feed(piece, i as u64 * 10);
    }
    let _ = engine.tick(999_999);
}

/// T-0017 mis-detection review: adversarial shapes through EVERY adapter.
/// A `?` in code, an open editor, or spinner output must never flip any
/// adapter to Question — the honest `unknown`/non-question path.
#[test]
fn adversarial_shapes_fool_no_adapter() {
    use arreo_core::state::{Adapter, Engine, State};
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../adapters");
    let adapters = ["default.toml", "pi.toml", "opencode.toml", "omp.toml"];
    // Shapes: ?-heavy code, vim alt-screen, spinner storm, bell-less noise.
    let shapes: &[&[u8]] = &[
        b"fn f() {\n  // what? why? huh?\n  let x = a ? b : c;\n  Ok(x?)\n}\n",
        b"\x1b[?1049h\x1b[1;1H~   vim   \x1b[24;1H\"file\" 1L, 17B",
        b"loading 1\rloading 2\rloading 3\rloading 4\r",
        b"Some(Coffee { ml: 330 }) // no question here, just code\n",
    ];
    for name in adapters {
        let adapter = Adapter::load(&root.join(name)).expect("adapter loads");
        for (i, shape) in shapes.iter().enumerate() {
            let mut engine = Engine::new(adapter.clone(), 0);
            engine.feed(shape, 0);
            let later = engine.feed(b"", 5_000);
            assert!(
                !later.iter().any(|e| e.state == State::Question),
                "{name} shape {i}: false question: {later:?}"
            );
        }
    }
}

/// Native-tier payload honesty: opencode's recorded permission line yields
/// `question (inferred)` WITH the matched pattern naming the scope.
#[test]
fn opencode_permission_line_carries_its_pattern() {
    use arreo_core::state::{Adapter, Engine, State};
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../adapters");
    let adapter = Adapter::load(&root.join("opencode.toml")).expect("opencode loads");
    let mut engine = Engine::new(adapter, 0);
    engine.feed(
        b"! permission requested: external_directory (/tmp/x/*); auto-rejecting\n",
        0,
    );
    let later = engine.feed(b"", 2_500);
    let question = later
        .iter()
        .find(|e| e.state == State::Question)
        .expect("asks: {later:?}");
    assert!(
        question
            .matched_pattern
            .as_deref()
            .is_some_and(|p| p.contains("permission requested")),
        "pattern names the scope: {:?}",
        question.matched_pattern
    );
}

/// T-0072: the `[resume]` table is validated as strictly as the patterns. Each
/// case below looks like a working strategy and silently is not, so each must
/// be a loud parse failure rather than a pane that quietly stops resuming.
#[test]
fn resume_strategies_are_validated_loudly() {
    let base = "idle_after_ms = 2000\nquestion_after_ms = 2000\nblocked_after_ms = 2500\n\
                question_patterns = ['x']\nerror_patterns = ['y']\n\
                harness = 'h'\nprograms = ['h']\n";
    let bad = [
        // Not a strategy at all.
        "[resume]\nkind = 'guess'\nargv = ['--continue']\n",
        // A `pin` argv that cannot name the session it pinned.
        "[resume]\nkind = 'pin'\nargv = ['--continue']\n",
        // A `continue` argv handed an id the harness does not take it for.
        "[resume]\nkind = 'continue'\nargv = ['--session', '{session}']\n",
        // `exact_argv` where the id is always known: dead data.
        "[resume]\nkind = 'pin'\nargv = ['--session-id', '{session}']\nexact_argv = ['-s', '{session}']\n",
        // `exact_argv` that cannot carry the id.
        "[resume]\nkind = 'continue'\nargv = ['--continue']\nexact_argv = ['--session', 'x']\n",
        // A placeholder embedded in a larger word would reach the harness literally.
        "[resume]\nkind = 'pin'\nargv = ['--session-id={session}']\n",
        // Two ids in one template: which is the session?
        "[resume]\nkind = 'pin'\nargv = ['--session-id', '{session}', '--name', '{session}']\n",
        // Empty argv: nothing to resume with.
        "[resume]\nkind = 'continue'\nargv = []\n",
        // An empty element in the argv list.
        "[resume]\nkind = 'continue'\nargv = ['']\n",
        // A pattern with two capture groups leaves "which is the id" to convention.
        "[resume]\nkind = 'continue'\nargv = ['--continue']\nsession_pattern = '\"a\":\"(x)\",\"id\":\"(y)\"'\n",
        // A pattern with no group can match without ever yielding a session.
        "[resume]\nkind = 'continue'\nargv = ['--continue']\nsession_pattern = 'ses_[0-9]+'\n",
        // A pattern that is not a regex.
        "[resume]\nkind = 'continue'\nargv = ['--continue']\nsession_pattern = '(['\n",
        // An unknown key inside the table is a typo, not a feature.
        "[resume]\nkind = 'continue'\nargv = ['--continue']\nresume_flag = '--resume'\n",
    ];
    for (i, table) in bad.iter().enumerate() {
        let text = format!("{base}{table}");
        assert!(
            Adapter::from_toml(&text).is_err(),
            "case {i} must be refused:\n{table}"
        );
    }

    // And the honest shapes are accepted, with the strategy readable.
    let pin = Adapter::from_toml(&format!(
        "{base}[resume]\nkind = 'pin'\nargv = ['--session-id', '{{session}}']\n"
    ))
    .expect("a pin strategy is valid");
    let resume = pin.resume.as_ref().expect("strategy kept");
    assert_eq!(resume.kind(), arreo_core::state::ResumeKind::Pin);
    assert_eq!(resume.argv(), ["--session-id", "{session}"]);
    assert!(!pin.captures_session(), "no pattern declared");

    let continuation = Adapter::from_toml(&format!(
        "{base}[resume]\nkind = 'continue'\nargv = ['--continue']\n\
         exact_argv = ['--session', '{{session}}']\nsession_pattern = '\"sessionID\":\"(ses_[A-Za-z0-9]+)\"'\n"
    ))
    .expect("a continue strategy is valid");
    let resume = continuation.resume.as_ref().expect("strategy kept");
    assert_eq!(resume.kind(), arreo_core::state::ResumeKind::Continue);
    assert_eq!(
        resume.exact_argv(),
        Some(["--session".to_string(), "{session}".to_string()].as_slice())
    );
    assert!(continuation.captures_session());

    // A `[resume]` with no harness id can never be resolved from a record.
    assert!(Adapter::from_toml(
        "idle_after_ms = 2000\nquestion_after_ms = 2000\nblocked_after_ms = 2500\n\
         question_patterns = ['x']\nerror_patterns = ['y']\n\
         [resume]\nkind = 'continue'\nargv = ['--continue']\n"
    )
    .is_err());
    // A harness with no programs could never be selected for a pane.
    assert!(Adapter::from_toml(
        "idle_after_ms = 2000\nquestion_after_ms = 2000\nblocked_after_ms = 2500\n\
         question_patterns = ['x']\nerror_patterns = ['y']\nharness = 'h'\n"
    )
    .is_err());
    // And programs with no harness would record panes under a harness
    // that does not exist.
    assert!(Adapter::from_toml(
        "idle_after_ms = 2000\nquestion_after_ms = 2000\nblocked_after_ms = 2500\n\
         question_patterns = ['x']\nerror_patterns = ['y']\nprograms = ['h']\n"
    )
    .is_err());
}

/// The two operations the restore path is built on are inverse: what a spawn
/// wrote down can be read back and re-applied without growing the argv.
#[test]
fn resume_args_and_base_args_are_inverse() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../adapters");
    // pi: pin.
    let pi = Adapter::load(&root.join("pi.toml")).expect("pi loads");
    let resume = pi.resume.as_ref().expect("pi resumes");
    let base = vec!["-p".to_string(), "hi".to_string()];
    let id = "01a099cd-2d35-74c4-90c6-d0e3b10011b5";
    let spawned = resume.resume_args(&base, Some(id)).expect("pinned");
    assert_eq!(spawned, ["-p", "hi", "--session-id", id]);
    let (back, carried) = resume.base_args(&spawned);
    assert_eq!(back, base);
    assert_eq!(carried.as_deref(), Some(id));
    assert_eq!(
        resume.resume_args(&back, carried.as_deref()),
        Some(spawned.clone()),
        "a second restore must not stack the flag"
    );
    // A pin with no id has nothing to pin — and must not fabricate one.
    assert_eq!(resume.resume_args(&base, None), None);

    // opencode: continue, with and without an id.
    let opencode = Adapter::load(&root.join("opencode.toml")).expect("opencode loads");
    let resume = opencode.resume.as_ref().expect("opencode resumes");
    let base = vec!["--dir".to_string(), "/w".to_string()];
    assert_eq!(
        resume.resume_args(&base, None),
        Some(vec![
            "--dir".to_string(),
            "/w".to_string(),
            "--continue".to_string()
        ])
    );
    let exact = resume
        .resume_args(&base, Some("ses_abc123"))
        .expect("exact");
    assert_eq!(exact, ["--dir", "/w", "--session", "ses_abc123"]);
    let (back, carried) = resume.base_args(&exact);
    assert_eq!(back, base);
    assert_eq!(carried.as_deref(), Some("ses_abc123"));
    assert_eq!(resume.resume_args(&back, carried.as_deref()), Some(exact));
    // And the continuation form round-trips too: no id must not be invented.
    let (back, carried) = resume.base_args(&resume.resume_args(&base, None).expect("continue"));
    assert_eq!(back, base);
    assert_eq!(carried, None);

    // omp (T-0080): same lineage as pi's envelope but no `--session-id` —
    // `-c` continues, `-r {session}` resumes by id prefix, and both re-open
    // the same file (verified live, 18.1.16, omp-resume2.txt).
    let omp = Adapter::load(&root.join("omp.toml")).expect("omp loads");
    let resume = omp.resume.as_ref().expect("omp resumes");
    assert_eq!(resume.kind(), arreo_core::state::ResumeKind::Continue);
    let base = vec!["-p".to_string(), "hi".to_string()];
    assert_eq!(
        resume.resume_args(&base, None),
        Some(vec!["-p".to_string(), "hi".to_string(), "-c".to_string()])
    );
    let id = "01a099e5-e66a-7568-9bd6-ac330e14b488";
    let exact = resume
        .resume_args(&base, Some(id))
        .expect("exact by prefix");
    assert_eq!(exact, ["-p", "hi", "-r", id]);
    let (back, carried) = resume.base_args(&exact);
    assert_eq!(back, base);
    assert_eq!(carried.as_deref(), Some(id));
    // A restore on a restored record must not stack a second `-r`.
    assert_eq!(
        resume.resume_args(&back, carried.as_deref()),
        Some(exact),
        "a second restore must not stack another -r"
    );
}

/// T-0072 capture: the id is learned where the harness prints it. pi's JSON
/// session envelope and opencode's event stream are the two recorded shapes;
/// an adapter with no pattern (or a harness that prints nothing) learns none,
/// which is not an error — the `continue` strategy resumes without one.
#[test]
fn engines_capture_the_session_id_from_real_output() {
    use arreo_core::state::{Adapter, Engine};
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../adapters");

    // pi (--mode json): the first line is the session envelope. Split across
    // two feeds on purpose: a session line can straddle a poll.
    let pi = Adapter::load(&root.join("pi.toml")).expect("pi loads");
    let mut engine = Engine::new(pi, 0);
    assert_eq!(engine.session(), None, "nothing learned before output");
    engine.feed(b"{\"type\":\"session\",\"version\":3,\"id\":\"", 0);
    assert_eq!(engine.session(), None, "a partial id is not an id");
    engine.feed(
        b"01a099cd-2d35-74c4-90c6-d0e3b10011b5\",\"cwd\":\"/w\"}\n",
        10,
    );
    assert_eq!(
        engine.session(),
        Some("01a099cd-2d35-74c4-90c6-d0e3b10011b5"),
        "pi's envelope carries the id"
    );
    assert!(engine.take_session_learned(), "learned from output");
    assert!(!engine.take_session_learned(), "and only once");

    // A pinned id is not overwritten by whatever output says.
    let pi = Adapter::load(&root.join("pi.toml")).expect("pi loads");
    let mut engine = Engine::new(pi, 0);
    engine.set_session("pinned-id".to_string());
    engine.feed(
        b"{\"type\":\"session\",\"id\":\"other-id-0000-0000-0000-000000000000\"}\n",
        0,
    );
    assert_eq!(engine.session(), Some("pinned-id"));
    assert!(!engine.take_session_learned(), "a pin is not a capture");

    // opencode (--format json): events carry "sessionID":"ses_…".
    let opencode = Adapter::load(&root.join("opencode.toml")).expect("opencode loads");
    let mut engine = Engine::new(opencode, 0);
    engine.feed(
        b"{\"type\":\"step_start\",\"sessionID\":\"ses_0a1b2C3d4E\",\"part\":{}}\n",
        0,
    );
    assert_eq!(engine.session(), Some("ses_0a1b2C3d4E"));

    // An interactive opencode TUI prints no id (verified live, 1.18.30): the
    // engine learns nothing and that is not an error.
    let opencode = Adapter::load(&root.join("opencode.toml")).expect("opencode loads");
    let mut engine = Engine::new(opencode, 0);
    engine.feed(b"opencode\n> ask anything\n", 0);
    assert_eq!(engine.session(), None);
    assert!(!engine.take_session_learned());

    // omp (T-0080): same NDJSON session envelope as pi (18.1.16, labels its
    // resume `-r {session}`), so the capture pattern matches the same shape.
    let omp = Adapter::load(&root.join("omp.toml")).expect("omp loads");
    let mut engine = Engine::new(omp, 0);
    engine.feed(b"{\"type\":\"session\",\"version\":3,\"id\":\"", 0);
    assert_eq!(engine.session(), None, "a partial id is not an id");
    engine.feed(
        b"01a099e5-e66a-7568-9bd6-ac330e14b488\",\"cwd\":\"/w\"}\n",
        10,
    );
    assert_eq!(
        engine.session(),
        Some("01a099e5-e66a-7568-9bd6-ac330e14b488"),
        "omp's envelope carries the id"
    );

    // The universal adapter has no strategy and captures nothing.
    let mut engine = Engine::new(Adapter::default(), 0);
    engine.feed(
        b"{\"type\":\"session\",\"id\":\"01a099cd-2d35-74c4-90c6-d0e3b10011b5\"}\n",
        0,
    );
    assert_eq!(engine.session(), None);
}
