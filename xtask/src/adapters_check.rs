//! T-0017: `xtask adapters --check` — lint every adapter TOML + replay
//! every adapter's fixtures through its own config, asserting timelines.
//!
//! This is the registry gate: unknown fields, bad regexes, empty lists fail
//! loudly; each adapter must earn its patterns against RECORDED output.

use std::path::PathBuf;
use std::process::ExitCode;

fn adapters_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the workspace root")
        .join("adapters")
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the workspace root")
        .join("fixtures")
}

/// Adapter name → its fixture files (≥4 each: question/working/idle/stress).
fn adapter_fixtures(name: &str) -> Vec<&'static str> {
    match name {
        "pi" => vec![
            "pi-question.pty",
            "pi-working.pty",
            "pi-idle.pty",
            "pi-stress.pty",
        ],
        "opencode" => vec![
            "opencode-question.pty",
            "opencode-working.pty",
            "opencode-idle.pty",
            "opencode-stress.pty",
        ],
        "default" => vec![
            "question-permission.pty",
            "working-stream.pty",
            "idle-shell.pty",
            "vim-edit.pty",
        ],
        _ => vec![],
    }
}

/// Expected trajectory per fixture: (must_see_working_during_flow, end_state).
/// - working fixtures: output flows (Working seen) then ends (Idle after silence).
/// - question fixtures: end Question (prompt tail + silence).
/// - idle: ends Idle.
/// - stress: pi-stress GENUINELY ends with a model question (recorded tail:
///   "¿Querés que convierta...?") → Question is CORRECT there; opencode-stress
///   has no question → Idle. The mis-detection review asserts mid-line `?`
///   never fires mid-flow (unit tests), not that trailing questions vanish.
fn expected_trajectory(fixture: &str) -> (bool, arreo_core::state::State) {
    use arreo_core::state::State as S;
    match fixture {
        f if f.contains("question") => (true, S::Question),
        f if f.contains("working") => (true, S::Idle),
        f if f.contains("idle") => (false, S::Idle),
        "pi-stress.pty" => (true, S::Question),
        f if f.contains("stress") => (true, S::Idle),
        f if f.contains("vim") => (true, S::Idle),
        _ => (false, S::Unknown),
    }
}

pub fn run(rest: &[String]) -> ExitCode {
    if rest.iter().any(|a| a == "--help" || a == "-h") {
        println!("usage: xtask adapters --check");
        println!("  lints adapters/*.toml and replays each adapter's fixtures.");
        return ExitCode::SUCCESS;
    }
    let dir = adapters_dir();
    let mut failed = 0usize;
    let mut passed = 0usize;

    let mut tomls: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("adapters dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "toml"))
        .collect();
    tomls.sort();
    if tomls.is_empty() {
        eprintln!("adapters --check: no adapters/*.toml found");
        return ExitCode::FAILURE;
    }

    for path in &tomls {
        let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("?");
        // 1. Lint: parse + validate (unknown fields, bad regex, empties).
        let adapter = match arreo_core::state::Adapter::load(path) {
            Ok(adapter) => adapter,
            Err(e) => {
                println!("[FAIL] {name}: invalid TOML: {e}");
                failed += 1;
                continue;
            }
        };
        println!(
            "[PASS] {name}: schema valid ({} question + {} error patterns)",
            adapter.question_patterns.len(),
            adapter.error_patterns.len()
        );
        passed += 1;

        // 2. Replay each fixture through THIS adapter; assert end state +
        // latency (first event timestamp ≤ 200 ms by construction — the
        // engine emits output events at feed time; assert it anyway).
        for fixture in adapter_fixtures(name) {
            let fpath = fixtures_dir().join(fixture);
            if !fpath.exists() {
                println!("[FAIL] {name}: missing fixture {fixture}");
                failed += 1;
                continue;
            }
            let loaded = match arreo_core::fixtures::Fixture::load(&fpath) {
                Ok(loaded) => loaded,
                Err(e) => {
                    println!("[FAIL] {name}: cannot load {fixture}: {e}");
                    failed += 1;
                    continue;
                }
            };
            let mut engine = arreo_core::state::Engine::new(adapter.clone(), 0);
            let raw = loaded.replay_accelerated();
            let mut t = 0u64;
            let mut first_latency: Option<u64> = None;
            let mut saw_working = false;
            for chunk in raw.chunks(512) {
                for event in engine.feed(chunk, t) {
                    if first_latency.is_none() {
                        first_latency = Some(event.t_ms.saturating_sub(t));
                    }
                    if matches!(event.state, arreo_core::state::State::Working) {
                        saw_working = true;
                    }
                }
                t += 50;
            }
            for event in engine.feed(b"", t + 5_000) {
                let _ = event;
            }
            let end = *engine.state();
            let (want_flow, want_end) = expected_trajectory(fixture);
            let mut ok = true;
            if want_flow && !saw_working {
                println!("[FAIL] {name}/{fixture}: never Working during flow");
                ok = false;
            }
            if end != want_end {
                println!("[FAIL] {name}/{fixture}: end {end:?}, want {want_end:?}");
                ok = false;
            }
            if ok {
                println!("[PASS] {name}/{fixture}: flow Working, end {end:?}");
                passed += 1;
            } else {
                failed += 1;
            }
            if let Some(latency) = first_latency {
                if latency > 200 {
                    println!("[FAIL] {name}/{fixture}: latency {latency} ms > 200 ms");
                    failed += 1;
                }
            }
        }
    }
    println!("adapters: {passed} passed, {failed} failed");
    if failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
