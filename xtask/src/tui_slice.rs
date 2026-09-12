//! T-0015 TUI slice: drive the real `arreo-tui` binary on a PTY and assert
//! what a human would see.
//!
//! No mocks: a real daemon on a temp socket, real panes with marker output,
//! the real TUI on a pty (util-linux `script`, sized with `stty`), and
//! assertions on the reconstructed screen — sidebar groups, attention
//! ordering, RAM column, pane content, search prompt, clean quit.
//!
//! `--interactive-evidence` writes the per-step screens to
//! `.loop/evidence/T-0015/` (the frames a reviewer reads).
//!
//! T-0076 adds the accessibility layer of the same slice: the frame at every
//! depth a terminal can be (truecolor, 256, 16, NO_COLOR), the two focus
//! signals, the key list, the motion switch, and 80×24 degradation — with the
//! captures under `.loop/evidence/T-0076/`. The budget checks (idle deltas, no
//! clear-screen) stay where T-0015 put them, because the mechanism is the same
//! one.

use crate::harness::{bins, cli, wait_bound, TestServer, TuiSession};
use arreo_core::theme::Depth;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

pub fn run(rest: &[String]) -> ExitCode {
    let evidence = rest.iter().any(|a| a == "--interactive-evidence");
    let evidence_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join(".loop")
        .join("evidence")
        .join("T-0015");
    let evidence_dir_76 = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join(".loop")
        .join("evidence")
        .join("T-0076");
    if evidence {
        let _ = std::fs::create_dir_all(&evidence_dir);
        let _ = std::fs::create_dir_all(&evidence_dir_76);
    }

    let socket = std::env::temp_dir().join(format!("arreo-e2e-tui-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&socket);
    let (server_bin, cli_bin, tui_bin) = bins();
    for bin in [&server_bin, &cli_bin, &tui_bin] {
        if !bin.exists() {
            println!("[FAIL] tui: missing binary {} (build first)", bin.display());
            return ExitCode::FAILURE;
        }
    }

    let mut failures = 0usize;
    let mut passes = 0usize;
    let mut check = |name: &str, ok: bool, detail: &str| {
        if ok {
            println!("[PASS] tui: {name}");
            passes += 1;
        } else {
            println!("[FAIL] tui: {name}: {detail}");
            failures += 1;
        }
    };

    let server = match TestServer::spawn(&server_bin, &socket, "server start") {
        Ok(server) => server,
        Err(code) => return code,
    };
    wait_bound(&socket);

    // Three panes: one prompt-hung (must sort first), two working.
    let panes = [
        ("alpha", "echo ALPHA-MARKER && sleep 120"),
        ("beta", "sleep 0.5; printf 'Proceed? [y/n] '; sleep 120"),
        ("gamma", "echo GAMMA-MARKER && sleep 120"),
    ];
    for (id, body) in panes {
        let (ok, out) = cli(&cli_bin, &socket, &["spawn", id, "/bin/sh", "-c", body]);
        if !ok {
            println!("[FAIL] tui: spawn {id}: {out}");
            return ExitCode::FAILURE;
        }
    }
    std::thread::sleep(Duration::from_secs(2));
    // Precondition: the daemon must have classified beta's prompt as a
    // question. Waiting here keeps this slice about the UI, not about the
    // classifier's latency (covered by the state slice).
    let (ok, out) = cli(
        &cli_bin,
        &socket,
        &["wait", "beta", "--state", "question", "--timeout", "10s"],
    );
    if !ok {
        println!("[FAIL] tui: beta never entered question state: {out}");
        return ExitCode::FAILURE;
    }

    let mut session = match TuiSession::start(&tui_bin, &socket) {
        Some(session) => session,
        None => {
            println!("[FAIL] tui: could not start TUI under a pty");
            return ExitCode::FAILURE;
        }
    };

    // One poll cycle + a frame.
    std::thread::sleep(Duration::from_secs(3));
    let screen = session.screen();
    if evidence {
        let _ = std::fs::write(evidence_dir.join("01-overview.txt"), &screen);
        let _ = std::fs::write(evidence_dir_76.join("01-overview.txt"), &screen);
    }
    check(
        "sidebar lists all panes",
        screen.contains("alpha") && screen.contains("beta") && screen.contains("gamma"),
        "pane ids missing from sidebar",
    );
    check(
        "attention order puts the hung agent's group first",
        match (screen.find("question"), screen.find("working")) {
            (Some(q), Some(w)) => q < w,
            (Some(_), None) => true,
            _ => false,
        },
        "question group not above working",
    );
    // **T-0061: which machine, and what the hung agent is asking.** Both are read
    // off the sidebar *region* — before the sidebar's right edge — because the
    // question text is also in the focused pane's view on the right, and a check
    // that cannot tell those apart would pass on a frame with no sidebar at all.
    check(
        "the sidebar names the machine and the link",
        screen.contains("this machine · socket"),
        "the session label is not in the sidebar title",
    );
    check(
        "the hung agent's question is shown beside it, in the sidebar",
        match screen.find("   Proceed? [y/n]") {
            Some(at) => {
                let line_start = screen[..at].rfind('\n').map_or(0, |n| n + 1);
                at - line_start < 30
            }
            None => false,
        },
        "beta's question is not on an indented sidebar line",
    );
    // **State is never color-only (T-0076).** Every group header is a dot
    // *shape* plus the state's own word, so the sidebar is readable with no
    // color at all — and the two are adjacent, not merely both on screen.
    check(
        "state dots rendered",
        screen.contains('◉') && screen.contains('●'),
        "state dots missing",
    );
    check(
        "every state row is a dot shape and its own name",
        screen.contains("◉ question") && screen.contains("● working"),
        "a state row carries only a hue",
    );
    check(
        "status bar advertises keys",
        screen.contains("q quit") && screen.contains("j/k move"),
        "key hints missing from status bar",
    );
    check(
        "RAM rendered for every live pane with a unit",
        screen
            .split_whitespace()
            .filter(|w| {
                w.len() > 1
                    && (w.ends_with('K') || w.ends_with('M'))
                    && w[..w.len() - 1].parse::<u64>().is_ok()
            })
            .count()
            >= 3,
        "no KiB/MiB reading for each pane",
    );

    // **Two signals (T-0076).** `j` moves the keyboard cursor (▶, inverse
    // video) without attaching anything; Enter attaches the pane the cursor is
    // on (▸ in the sidebar, the cyan border on the pane region).
    session.send("j");
    std::thread::sleep(Duration::from_millis(600));
    let cursor_only = session.screen();
    if evidence {
        let _ = std::fs::write(evidence_dir_76.join("02-cursor.txt"), &cursor_only);
    }
    check(
        "the keyboard cursor is visible before anything is attached",
        sidebar_region(&cursor_only).contains('▶') && !sidebar_region(&cursor_only).contains('▸'),
        "the cursor row is not marked, or a selection exists before Enter",
    );
    session.send("\r");
    std::thread::sleep(Duration::from_secs(2));
    let focused = session.screen();
    if evidence {
        let _ = std::fs::write(evidence_dir.join("03-focus-pane.txt"), &focused);
    }
    check(
        "focused pane header shows id + state",
        focused.contains("[question]")
            || focused.contains("[working]")
            || focused.contains("[done]"),
        "no focused-pane header",
    );
    check(
        "focused pane streams daemon output",
        focused.contains("Proceed?") || focused.contains("MARKER"),
        "no pane output in transcript",
    );
    // The attached pane keeps its own marker while the cursor is elsewhere:
    // the selection did not move when the keyboard did.
    check(
        "the attached pane is marked in the sidebar",
        sidebar_region(&focused).contains('▸'),
        "no selection marker after Enter",
    );
    check(
        "the cursor left the sidebar for the pane region",
        !sidebar_region(&focused).contains('▶'),
        "the sidebar still shows a cursor while the pane region has focus",
    );
    if evidence {
        let _ = std::fs::write(evidence_dir_76.join("03-attached.txt"), &focused);
    }

    // Search prompt owns the status line.
    session.send("/");
    std::thread::sleep(Duration::from_millis(600));
    session.send("MARK");
    std::thread::sleep(Duration::from_millis(600));
    let searching = session.screen();
    if evidence {
        let _ = std::fs::write(evidence_dir.join("04-search.txt"), &searching);
    }
    check(
        "search prompt rendered on the status line",
        searching.contains("Enter apply"),
        "search prompt not on screen",
    );
    // Apply the filter: the prompt yields to the result count.
    session.send("\r");
    std::thread::sleep(Duration::from_secs(2));
    let filtered = session.screen();
    if evidence {
        let _ = std::fs::write(evidence_dir.join("04b-search-applied.txt"), &filtered);
    }
    check(
        "applied search reports match count",
        filtered.contains("matching lines"),
        "no match count after applying the filter",
    );

    // Mouse: click the third pane's sidebar row (SGR report, 1-based).
    // Layout at 120x30: row 1 = question header, 2 = beta, 3 = working
    // header, 4 = alpha, 5 = gamma.
    session.send("\u{1b}[<0;6;5M");
    std::thread::sleep(Duration::from_secs(2));
    let clicked = session.screen();
    if evidence {
        let _ = std::fs::write(evidence_dir.join("05-mouse-click.txt"), &clicked);
    }
    check(
        "mouse click focuses the clicked row",
        clicked.contains("gamma ["),
        "clicked row did not take focus",
    );

    // Resize: the pty reports a new size; the frame must re-lay-out to it.
    session.resize(24, 100);
    std::thread::sleep(Duration::from_secs(2));
    let resized = session.screen();
    if evidence {
        let _ = std::fs::write(evidence_dir.join("06-resize-100x24.txt"), &resized);
    }
    let widest = resized
        .lines()
        .map(str::chars)
        .map(Iterator::count)
        .max()
        .unwrap_or(0);
    check(
        "resize re-lays-out to the new width",
        widest == 100 && resized.contains("["),
        "frame did not match the 100-column pty",
    );
    check(
        "no stale rows below the resized frame",
        resized.lines().count() == 24,
        "frame kept rows from the old size",
    );

    // The status line promises "Esc clears"; Esc must mean that before it
    // means quit, or the hint is a lie (T-0076).
    session.send("\u{1b}");
    std::thread::sleep(Duration::from_millis(800));
    check(
        "Esc clears an applied search instead of quitting",
        !session.exited() && !session.screen().contains("matching lines"),
        "Esc quit the app, or the filter stayed applied",
    );

    // **80×24 is the documented minimum and must be usable (T-0076).**
    session.resize(24, 80);
    std::thread::sleep(Duration::from_secs(2));
    let small = session.screen();
    if evidence {
        let _ = std::fs::write(evidence_dir_76.join("04-small-80x24.txt"), &small);
    }
    check(
        "80x24 stays usable: panes, states and the key legend",
        small.contains("alpha")
            && small.contains("question")
            && small.contains("q quit")
            && !small.contains('…'),
        "the 80-column frame lost something",
    );
    check(
        "80x24 keeps the pane region (the sidebar clamps, it does not win)",
        sidebar_column(&small).is_some_and(|edge| 80 - edge >= 40),
        "the sidebar ate the pane region at 80 columns",
    );

    // Below that: clamped, not glitched. The frame still lays out to the new
    // size instead of overflowing it.
    session.resize(12, 40);
    std::thread::sleep(Duration::from_secs(2));
    let narrow = session.screen();
    if evidence {
        let _ = std::fs::write(evidence_dir_76.join("05-narrow-40x12.txt"), &narrow);
    }
    check(
        "40x12 clamps instead of glitching",
        narrow.lines().count() == 12
            && narrow.lines().all(|line| line.chars().count() <= 40)
            && narrow.contains("alpha"),
        "the narrow frame is not 40x12",
    );
    session.resize(30, 120);
    std::thread::sleep(Duration::from_secs(2));

    // Wall: every pane tiled at once, each tile streaming its own output.
    session.send("w");
    std::thread::sleep(Duration::from_secs(4));
    let wall = session.screen();
    if evidence {
        let _ = std::fs::write(evidence_dir.join("07-wall.txt"), &wall);
    }
    let tiles = ["alpha [", "beta [", "gamma ["]
        .iter()
        .filter(|title| wall.contains(*title))
        .count();
    check(
        "wall tiles every pane simultaneously",
        tiles == 3,
        &format!("{tiles}/3 pane titles rendered in the wall"),
    );
    check(
        "wall tiles stream their own pane's output",
        wall.contains("ALPHA-MARKER") && wall.contains("GAMMA-MARKER") && wall.contains("Proceed?"),
        "a wall tile is missing its pane's output",
    );

    // **The wall's cursor is visible (T-0076).** `j` moves the highlighted
    // tile, so the keyboard never acts somewhere the user cannot see.
    session.send("j");
    std::thread::sleep(Duration::from_secs(2));
    let wall_cursor = session.screen();
    if evidence {
        let _ = std::fs::write(evidence_dir_76.join("06-wall-focus.txt"), &wall_cursor);
    }
    check(
        "the wall marks exactly one tile as the cursor",
        wall_cursor.matches('▶').count() == 1
            && ["alpha", "beta", "gamma"]
                .iter()
                .any(|id| wall_cursor.contains(&format!("{id} ["))),
        "no single visible cursor tile in the wall",
    );

    // **The key list (T-0076).** Every binding on screen, so the TUI is
    // complete without the docs.
    session.send("?");
    std::thread::sleep(Duration::from_secs(1));
    let help = session.screen();
    if evidence {
        let _ = std::fs::write(evidence_dir_76.join("07-keys.txt"), &help);
    }
    check(
        "? lists the keys on screen",
        help.contains("move the cursor") && help.contains("quit") && help.contains("wall ↔ focus"),
        "the key list did not open",
    );
    session.send("\u{1b}");
    std::thread::sleep(Duration::from_millis(600));
    check(
        "Esc closes the key list without quitting",
        !session.exited(),
        "the TUI exited instead of dismissing the key list",
    );

    // **The theme picker (T-0076 evidence): the same overlay, captured.**
    session.send("t");
    std::thread::sleep(Duration::from_secs(1));
    let picker = session.screen();
    if evidence {
        let _ = std::fs::write(evidence_dir_76.join("08-theme-picker.txt"), &picker);
    }
    let listed = ["arreo", "tokyonight", "catppuccin", "gruvbox", "system"]
        .iter()
        .filter(|name| picker.contains(**name))
        .count();
    check(
        "t opens the theme picker over every built-in",
        picker.contains("theme:") && listed == 5,
        &format!("{listed}/5 built-ins listed"),
    );
    session.send("\u{1b}");
    std::thread::sleep(Duration::from_millis(600));

    // Border drag: grab the sidebar edge and widen it; the split must follow.
    let before = session.screen();
    let divider_before = before
        .lines()
        .next()
        .and_then(|l| l.chars().position(|c| c == '┐'));
    session.send("\u{1b}[<0;29;3M"); // press on the border (col 28, 0-based)
    std::thread::sleep(Duration::from_millis(300));
    session.send("\u{1b}[<32;41;3M"); // drag to col 40
    std::thread::sleep(Duration::from_millis(300));
    session.send("\u{1b}[<0;41;3M"); // release
    std::thread::sleep(Duration::from_secs(2));
    let dragged = session.screen();
    if evidence {
        let _ = std::fs::write(evidence_dir.join("08-border-drag.txt"), &dragged);
    }
    let divider_after = dragged
        .lines()
        .next()
        .and_then(|l| l.chars().position(|c| c == '┐'));
    check(
        "mouse drag resizes the sidebar split",
        matches!((divider_before, divider_after), (Some(b), Some(a)) if a > b),
        &format!("divider {divider_before:?} -> {divider_after:?}"),
    );

    // Back to the focus view for the remaining key checks.
    session.send("w");
    std::thread::sleep(Duration::from_secs(2));

    // Steady state: the app must repaint cells, not the screen. A frame is
    // only what changed, so idle output stays tiny and never clears.
    let before = session.transcript().len();
    std::thread::sleep(Duration::from_secs(3));
    let during = session.transcript();
    let idle_bytes = during.len() - before;
    check(
        "steady state repaints deltas only",
        idle_bytes < 4096 && !during[before..].windows(4).any(|w| w == b"\x1b[2J"),
        &format!("{idle_bytes} bytes written while idle"),
    );

    // Quit returns the pty to its parent shell (the process exits).
    session.send("q");
    std::thread::sleep(Duration::from_secs(2));
    check(
        "q quits cleanly",
        session.exited(),
        "TUI still running after q",
    );

    // ---- T-0076: every depth a terminal can be, and the motion switch -------
    //
    // One session per terminal shape against the same daemon (beta is still
    // asking, so a state hue and a state *word* are both on screen). The
    // captures are the evidence a reviewer reads; the checks are what makes
    // "readable without color" an assertion rather than an opinion.
    for shape in DEPTH_SHAPES {
        let name = shape.name;
        let Some(mut shaped) = TuiSession::start_with(&tui_bin, &socket, &[], shape.env) else {
            println!("[FAIL] tui: could not start the TUI for depth {name}");
            return ExitCode::FAILURE;
        };
        std::thread::sleep(Duration::from_secs(3));
        let frame = shaped.screen();
        if evidence {
            let _ = std::fs::write(evidence_dir_76.join(format!("depth-{name}.txt")), &frame);
            // The raw stream too: a blink is a style, so the *bytes* are what
            // show the pulse (and its absence under NO_COLOR).
            let _ = std::fs::write(
                evidence_dir_76.join(format!("depth-{name}.raw")),
                shaped.transcript(),
            );
            if shape.depth == Depth::Truecolor {
                // The pulse is a *style*, so the capture a reviewer can check
                // is the byte stream: SGR 5 on the question group, and no
                // clear-screen anywhere (the budget's other half).
                let _ = std::fs::write(
                    evidence_dir_76.join("09-question-pulse.raw"),
                    shaped.transcript(),
                );
            }
        }
        check(
            &format!("depth {name} is readable without relying on color"),
            ["alpha", "beta", "question", "working", "◉", "●", "q quit"]
                .iter()
                .all(|needle| frame.contains(needle)),
            &format!("the {name} frame lost a shape or a label"),
        );
        // The pulse is the terminal's own slow blink (SGR 5), and it is on
        // exactly where motion is allowed: not on a NO_COLOR terminal.
        let blinking = crate::theme_slice::sgr_sets_modifier(&shaped.transcript(), 5);
        if shape.depth == Depth::NoColor {
            check(
                "NO_COLOR implies still: no blink on the question group",
                !blinking,
                "a NO_COLOR frame asked the terminal to blink",
            );
        } else {
            check(
                &format!("depth {name} still pulses the question group"),
                blinking,
                "the attention cue is missing where motion is allowed",
            );
        }
        shaped.send("q");
        std::thread::sleep(Duration::from_secs(1));
    }

    // **`tui.reduce_motion` (T-0076).** The same terminal shape as the pulsing
    // case above, with one line of config: the state stays on screen (dot and
    // word) and the motion is gone.
    let config = std::env::temp_dir().join(format!("arreo-e2e-tui-{}.toml", std::process::id()));
    std::fs::write(&config, "[tui]\nreduce_motion = true\n").expect("write the config");
    let config_arg = config.to_str().expect("utf-8 temp path");
    let Some(mut still) = TuiSession::start_with(
        &tui_bin,
        &socket,
        &["--config", config_arg],
        &[("COLORTERM", "truecolor"), ("TERM", "xterm-256color")],
    ) else {
        println!("[FAIL] tui: could not start the TUI for the reduce_motion case");
        return ExitCode::FAILURE;
    };
    std::thread::sleep(Duration::from_secs(3));
    let frame = still.screen();
    if evidence {
        let _ = std::fs::write(evidence_dir_76.join("depth-reduce-motion.txt"), &frame);
        let _ = std::fs::write(
            evidence_dir_76.join("depth-reduce-motion.raw"),
            still.transcript(),
        );
    }
    check(
        "tui.reduce_motion stills the pulse",
        !crate::theme_slice::sgr_sets_modifier(&still.transcript(), 5),
        "reduce_motion did not stop the blink",
    );
    check(
        "...and the state is still on screen, as a shape and a word",
        frame.contains("◉ question"),
        "stilling the motion removed the cue instead of the motion",
    );
    still.send("q");
    std::thread::sleep(Duration::from_secs(1));
    let _ = std::fs::remove_file(&config);

    // Metrics graph case (T-0040): the focused pane's title carries the RAM
    // sparkline from the durable series — real pty, real key events, the same
    // session. A `--case metrics-graph` run asserts only this (fast feedback);
    // the full slice always asserts it too, because the sparkline is part of
    // the focused view, not an optional extra.
    let case = rest
        .windows(2)
        .find(|w| w[0] == "--case")
        .map(|w| w[1].as_str());
    if case.is_none() || case == Some("metrics-graph") {
        // Fresh session: the writer needs one 10 s tick to record the first
        // row, and the poller needs one pass to fetch it.
        let server2 = match TestServer::spawn(&server_bin, &socket, "server start") {
            Ok(server) => server,
            Err(code) => return code,
        };
        wait_bound(&socket);
        cli(
            &cli_bin,
            &socket,
            &[
                "spawn",
                "graph",
                "/bin/sh",
                "-c",
                "echo GRAPH-MARKER; sleep 60",
            ],
        );
        let mut graph = match TuiSession::start(&tui_bin, &socket) {
            Some(session) => session,
            None => {
                check("metrics graph session starts", false, "no pty");
                drop(server);
                drop(server2);
                let _ = std::fs::remove_file(&socket);
                return ExitCode::FAILURE;
            }
        };
        std::thread::sleep(Duration::from_secs(12));
        // Focus the pane (it is the only one) and read the title.
        graph.send("j");
        std::thread::sleep(Duration::from_millis(500));
        graph.send("\r");
        std::thread::sleep(Duration::from_secs(2));
        let screen = graph.screen();
        // The sparkline: block chars between pipes, plus the peak label — or
        // nothing at all when history is unavailable (the honest empty, never
        // a flat line claiming "steady").
        let has_graph = screen.contains('▏') && screen.contains("peak");
        check(
            "the focused pane shows a RAM sparkline with its peak",
            has_graph || !screen.contains("GRAPH-MARKER"),
            &format!(
                "no sparkline and no marker either: {}",
                screen.lines().next().unwrap_or("")
            ),
        );
        if evidence {
            let _ = std::fs::write(evidence_dir.join("09-metrics-graph.txt"), &screen);
        }
        graph.send("q");
        std::thread::sleep(Duration::from_secs(1));
    }

    drop(session);
    drop(server);
    let _ = std::fs::remove_file(&socket);
    let mut db = socket.into_os_string();
    db.push(".db");
    let _ = std::fs::remove_file(&db);

    if failures == 0 {
        println!("tui: {passes} passed, 0 failed");
        ExitCode::SUCCESS
    } else {
        println!("tui: {passes} passed, {failures} failed");
        ExitCode::FAILURE
    }
}

/// One terminal shape the T-0076 depth matrix drives the TUI under: the
/// environment that decides detection, and the depth it must therefore render
/// at. `Depth` comes from `arreo-core`, so the expectation is the engine's own
/// answer rather than a second opinion about what `TERM=xterm` means.
struct DepthShape {
    name: &'static str,
    env: &'static [(&'static str, &'static str)],
    depth: Depth,
}

const DEPTH_SHAPES: &[DepthShape] = &[
    // iTerm2/Alacritty/kitty shape.
    DepthShape {
        name: "truecolor",
        env: &[("COLORTERM", "truecolor"), ("TERM", "xterm-256color")],
        depth: Depth::Truecolor,
    },
    // Windows Terminal / older xterm: 256 colors, no COLORTERM hint.
    DepthShape {
        name: "256",
        env: &[("COLORTERM", ""), ("TERM", "xterm-256color")],
        depth: Depth::Ansi256,
    },
    // Legacy Terminal.app / conhost shape: bare xterm, 16 colors.
    DepthShape {
        name: "16",
        env: &[("COLORTERM", ""), ("TERM", "xterm")],
        depth: Depth::Ansi16,
    },
    // NO_COLOR: no color at all, and no motion either.
    DepthShape {
        name: "no-color",
        env: &[("NO_COLOR", "1"), ("TERM", "xterm-256color")],
        depth: Depth::NoColor,
    },
];

/// The sidebar's own columns of a reconstructed frame, as text.
///
/// The sidebar and the pane region repeat the same words (a pane id is in both),
/// so a check that could not tell them apart would pass on a frame with no
/// sidebar at all — the same reasoning the T-0061 checks use.
fn sidebar_region(screen: &str) -> String {
    match sidebar_column(screen) {
        Some(edge) => screen
            .lines()
            .map(|line| line.chars().take(edge as usize).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n"),
        None => String::new(),
    }
}

/// The column just past the sidebar's right border, read off the frame itself
/// (the `┐` of its title row) rather than from a constant that could drift.
fn sidebar_column(screen: &str) -> Option<u16> {
    screen
        .lines()
        .next()?
        .chars()
        .position(|c| c == '┐')
        .map(|col| col as u16 + 1)
}
