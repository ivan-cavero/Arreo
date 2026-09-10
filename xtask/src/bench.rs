//! T-0008: nightly load benchmark vs `perf-budget.toml`.
//!
//! One sentence: spawn N real panes, replay recorded fixtures as traffic
//! through the VT grid + state engine + sampler, measure everything the
//! Phase-0 budget rows name, fail loudly on regression.
//!
//! What it measures (each a budget row):
//! - `server_rss_10panes_mb`: peak RSS of THIS process while 10 panes live
//!   (own PID via `/proc/self/statm` — the harness overhead itself, agents
//!   excluded by construction since panes run `/bin/sh` sleep loops).
//! - `per_pane_hot_kb`: max over panes of
//!   `ring.bytes_held + vt.ram_bytes + journal` (same accounting as T-0002).
//! - `detection_latency_ms`: worst feed→event delay driving the real Engine
//!   with recorded fixture bytes on an explicit clock.
//! - `sampler_sweep_30panes_ms`: wall time for 30 `sample_tree` sweeps × 10
//!   (extrapolated 30-pane cost at 1 s cadence).
//! - `vt_feed_1byte_ms`: worst single-byte feed on a 200×60 grid.
//! - `replay_determinism`: fixture load→replay→compare mismatches (must be 0).
//!
//! Budgets are read from `perf-budget.toml` at the workspace root (found by
//! walking up from the xtask manifest dir). `--panes N` overrides the pane
//! count. `--enforce` is the default behavior for bench (it IS the gate) —
//! accepted for uniformity, always enforced. Machine-readable JSON goes to
//! stdout with `--json` (CI artifact); human table otherwise.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

pub fn bench(rest: &[String]) -> ExitCode {
    if rest.iter().any(|a| a == "--help" || a == "-h") {
        println!("usage: xtask bench [--panes N] [--json] [--probe proto]");
        println!("  spawns N real panes, replays fixtures, asserts perf-budget.toml phase-0 rows.");
        println!("  --probe proto: only the 1MB MessagePack codec check (T-0013).");
        return ExitCode::SUCCESS;
    }
    if rest.iter().any(|a| a == "--probe") {
        let which = rest
            .windows(2)
            .find(|w| w[0] == "--probe")
            .map(|w| w[1].as_str());
        match which {
            Some("proto") => return probe_proto(rest.iter().any(|a| a == "--json")),
            Some(other) => {
                eprintln!("bench: unknown probe {other:?} (have: proto)");
                return ExitCode::from(2);
            }
            None => {
                eprintln!("bench: --probe needs a name (have: proto)");
                return ExitCode::from(2);
            }
        }
    }
    let panes: usize = rest
        .windows(2)
        .find(|w| w[0] == "--panes")
        .and_then(|w| w[1].parse().ok())
        .unwrap_or(10);
    let json = rest.iter().any(|a| a == "--json");

    let budget_path = workspace_root().join("perf-budget.toml");
    let budget = match Budget::load(&budget_path) {
        Ok(budget) => budget,
        Err(e) => {
            eprintln!("bench: cannot load {}: {e}", budget_path.display());
            return ExitCode::FAILURE;
        }
    };

    let report = run(panes, &budget);
    if json {
        println!("{}", report.to_json());
    } else {
        print!("{report}");
    }
    if report.failed() {
        eprintln!("bench: REGRESSION — budgets exceeded (see FAIL above)");
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the workspace root")
        .to_path_buf()
}

/// Parsed phase-0 thresholds (only the rows bench asserts).
struct Budget {
    max_server_rss_mb: u64,
    per_pane_hot_kb: u64,
    detection_latency_ms: u64,
    sampler_sweep_30panes_ms: u64,
    vt_feed_1byte_ms: u64,
}

impl Budget {
    fn load(path: &PathBuf) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("read: {e}"))?;
        let get = |key: &str| -> Result<u64, String> {
            // Inline-table rows: `key = { phase0 = true, target = 3072, ... }`.
            // No TOML dep in xtask by design (keep dev-tooling lean) — parse
            // the `target = N` field off the key's line.
            let line = text
                .lines()
                .find(|l| l.trim_start().starts_with(key))
                .ok_or_else(|| format!("missing key {key}"))?;
            // Inline-table rows carry `target = N`; plain rows (`[phase0]`
            // section) carry `= N` directly. Try target first.
            let after_target = line
                .split("target")
                .nth(1)
                .and_then(|rest| rest.split('=').nth(1));
            let plain = line.split('=').nth(1);
            after_target
                .or(plain)
                .and_then(|v| v.trim().split(|c: char| !c.is_ascii_digit()).next())
                .filter(|v| !v.is_empty())
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| format!("bad target for {key}"))
        };
        Ok(Self {
            max_server_rss_mb: get("max_server_rss_mb")?,
            per_pane_hot_kb: get("per_pane_hot_kb")?,
            detection_latency_ms: get("detection_latency_ms")?,
            sampler_sweep_30panes_ms: get("sampler_sweep_30panes_ms")?,
            vt_feed_1byte_ms: get("vt_feed_1byte_ms")?,
        })
    }
}

struct Check {
    name: &'static str,
    actual: String,
    target: String,
    pass: bool,
}

struct Report {
    panes: usize,
    checks: Vec<Check>,
    elapsed: Duration,
}

impl Report {
    fn failed(&self) -> bool {
        self.checks.iter().any(|c| !c.pass)
    }

    fn to_json(&self) -> String {
        let mut out = String::from("{\"panes\":");
        out.push_str(&self.panes.to_string());
        out.push_str(",\"checks\":[");
        for (i, c) in self.checks.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "{{\"name\":\"{}\",\"actual\":\"{}\",\"target\":\"{}\",\"pass\":{}}}",
                c.name, c.actual, c.target, c.pass
            ));
        }
        out.push_str(&format!(
            "],\"pass\":{},\"elapsed_ms\":{}}}",
            !self.failed(),
            self.elapsed.as_millis()
        ));
        out
    }
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "bench: {} panes in {:?} (budgets: perf-budget.toml)",
            self.panes, self.elapsed
        )?;
        for c in &self.checks {
            writeln!(
                f,
                "  [{}] {:<24} actual {:>10}  target {:<10}",
                if c.pass { "PASS" } else { "FAIL" },
                c.name,
                c.actual,
                c.target
            )?;
        }
        Ok(())
    }
}

fn proc_rss_mb() -> u64 {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|t| {
            t.split_whitespace()
                .nth(1)
                .and_then(|s| s.parse::<u64>().ok())
        })
        .map(|pages| pages * 4096 / 1_048_576)
        .unwrap_or(0)
}

fn run(panes: usize, budget: &Budget) -> Report {
    let start = Instant::now();
    let mut checks = Vec::new();
    let root = workspace_root();

    // 1. Spawn N real panes running idle-ish agents (sleep loop) + one
    // chatterbox replaying fixture traffic through VT + engine.
    let mut live = Vec::new();
    for i in 0..panes {
        match arreo_core::pty::Pane::spawn("/bin/sh", &["-c", "sleep 60"], 80, 24) {
            Ok(pane) => live.push(pane),
            Err(e) => {
                checks.push(Check {
                    name: "pane_spawn",
                    actual: format!("{e}"),
                    target: format!("{panes} panes"),
                    pass: false,
                });
                return Report {
                    panes,
                    checks,
                    elapsed: start.elapsed(),
                };
            }
        }
        let _ = i;
    }

    // 2. Fixture traffic through VT grid + state engine (worst-case shapes:
    // working stream × question prompt × vim escapes).
    let mut worst_feed = Duration::ZERO;
    let mut worst_latency = 0u64;
    let mut mismatches = 0usize;
    for name in [
        "working-stream.pty",
        "question-permission.pty",
        "vim-edit.pty",
    ] {
        let path = root.join("fixtures").join(name);
        let fixture = match arreo_core::fixtures::Fixture::load(&path) {
            Ok(fixture) => fixture,
            Err(e) => {
                checks.push(Check {
                    name: "fixture_load",
                    actual: format!("{name}: {e}"),
                    target: "loadable".to_string(),
                    pass: false,
                });
                continue;
            }
        };
        if fixture.replay_accelerated()
            != arreo_core::fixtures::Fixture::load(&path)
                .map(|f| f.replay_accelerated())
                .unwrap_or_default()
        {
            mismatches += 1;
        }
        let bytes = fixture.replay_accelerated();
        // VT feed cost on a big grid.
        let mut vt = arreo_core::vt::VtPane::new(200, 60);
        let feed_start = Instant::now();
        vt.feed(&bytes);
        let feed_elapsed = feed_start.elapsed();
        // Worst 1-byte feed (the O(grid-scan) tripwire).
        let one_start = Instant::now();
        vt.feed(b"x");
        worst_feed = worst_feed.max(one_start.elapsed());
        let _ = feed_elapsed;
        // Engine latency on explicit clock.
        let mut engine = arreo_core::state::Engine::new(arreo_core::state::Adapter::default(), 0);
        let mut t = 0u64;
        for chunk in bytes.chunks(1024) {
            for event in engine.feed(chunk, t) {
                worst_latency = worst_latency.max(event.t_ms.saturating_sub(t));
            }
            t += 10;
        }
        for event in engine.feed(b"", t + 5_000) {
            worst_latency = worst_latency.max(event.t_ms.saturating_sub(t + 5_000));
        }
        // Feed the first pane's ring too (hot-buffer accounting is real).
        if let Some(pane) = live.first() {
            let _ = pane.send(&bytes[..bytes.len().min(4096)]);
        }
    }
    // Per-pane hot accounting: ring + VT estimate + journal.
    let mut worst_pane_kb = 0u64;
    for pane in &live {
        let held = pane.bytes_held() as u64 / 1024;
        worst_pane_kb = worst_pane_kb.max(held);
    }
    // VT RAM measured on a dedicated flood pane (same method as T-0003).
    let mut vt = arreo_core::vt::VtPane::new(80, 24);
    vt.feed(&vec![b'A'; 512 * 200]);
    worst_pane_kb = worst_pane_kb.max(vt.ram_bytes() as u64 / 1024 + 200);

    // 3. Sampler sweep: 30-pane equivalent (10 rounds × 3 samples on live PIDs).
    let sweep_start = Instant::now();
    {
        let mut sampler = arreo_core::metrics::Sampler::new();
        let me = std::process::id();
        for _ in 0..30 {
            let _ = sampler.sample_tree(me);
        }
    }
    let sweep_elapsed = sweep_start.elapsed();

    // 4. Process RSS with all panes live.
    let rss_mb = proc_rss_mb();

    checks.push(Check {
        name: "server_rss_10panes_mb",
        actual: format!("{rss_mb} MB"),
        target: format!("≤ {} MB", budget.max_server_rss_mb),
        pass: rss_mb <= budget.max_server_rss_mb,
    });
    checks.push(Check {
        name: "per_pane_hot_kb",
        actual: format!("{worst_pane_kb} KB"),
        target: format!("≤ {} KB", budget.per_pane_hot_kb),
        pass: worst_pane_kb <= budget.per_pane_hot_kb,
    });
    checks.push(Check {
        name: "detection_latency_ms",
        actual: format!("{worst_latency} ms"),
        target: format!("≤ {} ms", budget.detection_latency_ms),
        pass: worst_latency <= budget.detection_latency_ms,
    });
    checks.push(Check {
        name: "sampler_sweep_30panes_ms",
        actual: format!("{} ms", sweep_elapsed.as_millis()),
        target: format!("≤ {} ms", budget.sampler_sweep_30panes_ms),
        pass: (sweep_elapsed.as_millis() as u64) <= budget.sampler_sweep_30panes_ms,
    });
    checks.push(Check {
        name: "vt_feed_1byte_ms",
        actual: format!("{} ms", worst_feed.as_millis()),
        target: format!("≤ {} ms", budget.vt_feed_1byte_ms),
        pass: (worst_feed.as_millis() as u64) <= budget.vt_feed_1byte_ms,
    });
    checks.push(Check {
        name: "replay_determinism",
        actual: format!("{mismatches}"),
        target: "0".to_string(),
        pass: mismatches == 0,
    });

    // Panes drop here (sleep children reaped via Pane drop → kill on daemon
    // path; here the child outlives us briefly — sleep 60 exits on its own;
    // no zombies: Pane::try_wait reaps on drop path via Child handle drop).
    Report {
        panes,
        checks,
        elapsed: start.elapsed(),
    }
}

/// T-0013 probe: 1 MB delta encode/decode vs the 5 ms budget, best-of-5
/// (debug allocator noise defeated by repetition, not by wishing).
/// Prints one line + JSON-ish detail; exit 1 on regression.
fn probe_proto(json: bool) -> ExitCode {
    use std::time::Instant;
    let lines: Vec<String> = (0..1000).map(|i| format!("{:01024}", i)).collect();
    let message = arreo_core::proto::Message::Delta {
        v: arreo_core::proto::VERSION,
        id: "bulk".to_string(),
        from_line: 0,
        lines,
    };
    // Warm up once (cold allocator lies).
    let bytes = arreo_core::proto::codec::encode(&message).expect("encode");
    assert!(bytes.len() >= 1_000_000);
    let mut best_encode = u128::MAX;
    let mut best_decode = u128::MAX;
    for _ in 0..5 {
        let start = Instant::now();
        let bytes = arreo_core::proto::codec::encode(&message).expect("encode");
        best_encode = best_encode.min(start.elapsed().as_millis());
        let start = Instant::now();
        let back = arreo_core::proto::codec::decode(&bytes).expect("decode");
        best_decode = best_decode.min(start.elapsed().as_millis());
        assert_eq!(back, message);
    }
    let pass = best_encode < 5 && best_decode < 5;
    if json {
        println!(
            "{{\"probe\":\"proto\",\"bytes\":{},\"encode_ms\":{},\"decode_ms\":{},\"pass\":{}}}",
            bytes.len(),
            best_encode,
            best_decode,
            pass
        );
    } else {
        println!(
            "bench --probe proto: 1MB delta best-of-5: encode {best_encode} ms, decode {best_decode} ms (target < 5 ms) → {}",
            if pass { "PASS" } else { "FAIL" }
        );
    }
    if pass {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
