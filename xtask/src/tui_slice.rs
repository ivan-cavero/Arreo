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
use arreo_core::proto::AgentState;
use arreo_core::theme::Depth;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

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
    // T-0079's captures: the 30-pane fixture's states and the measured first
    // frame. Written on every run, not only under `--interactive-evidence`: the
    // number this task exists to assert is evidence a reviewer reads, and a
    // measurement nobody can see after the fact is a claim.
    let evidence_dir_79 = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join(".loop")
        .join("evidence")
        .join("T-0079");
    let _ = std::fs::create_dir_all(&evidence_dir_79);
    // T-0073's captures: the opt-in exit, case by case — the frames a reviewer
    // reads for "the flag stops the daemon", "the live-pane prompt asks and `n`
    // aborts", and "a remote target is refused". Written on every run: the
    // confirmation is a *screen*, and a check that only asserts a substring
    // cannot show what the operator would have seen.
    let evidence_dir_73 = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join(".loop")
        .join("evidence")
        .join("T-0073");
    let _ = std::fs::create_dir_all(&evidence_dir_73);
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

    // Three panes, one per state the sidebar distinguishes (T-0079): one
    // prompt-hung (sorts first), one **genuinely working**, one quiet-but-alive
    // (idle). The fixture used to be three silent panes, which the old sidebar
    // reported as `working` for any live pane that was not asking — a
    // fabrication. Now that the sidebar is *told* the engine's state instead of
    // being handed a guess (see `arreo_tui::client::poll_summaries`), a silent
    // pane renders `idle`, because that is what the engine derives: output
    // flowing is `working`, two seconds of silence is `idle` (its own
    // `idle_after_ms`). So each pane below is the shape it claims to be — and
    // the frame this slice asserts carries all three dot shapes, which is more
    // than the old fixture ever put on screen. gamma's loop is bounded so the
    // pane outlives the slice but not the run.
    let panes = [
        ("alpha", "echo ALPHA-MARKER && sleep 120".to_string()),
        (
            "beta",
            "sleep 0.5; printf 'Proceed? [y/n] '; sleep 120".to_string(),
        ),
        (
            // **Genuinely working, not merely alive.** The cadence is 500 ms
            // against the engine's 2000 ms `idle_after_ms`: a slower one drifts
            // to `idle` whenever a poll, a `sleep` and a loaded box line up, and
            // a fixture that flaps between two states cannot assert either. The
            // marker repeats so the wall tile streams *live* output rather than
            // a first line that has long since scrolled off it.
            "gamma",
            "i=0; while [ $i -lt 220 ]; do echo \"GAMMA-MARKER $i\"; i=$((i+1)); sleep 0.5; done"
                .to_string(),
        ),
    ];
    for (id, body) in &panes {
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
    // Same precondition for the working pane (T-0079): it must have been
    // classified `working` before the frame is read, or the check below would be
    // racing the engine rather than asserting the UI.
    let (ok, out) = cli(
        &cli_bin,
        &socket,
        &["wait", "gamma", "--state", "working", "--timeout", "10s"],
    );
    if !ok {
        println!("[FAIL] tui: gamma never entered working state: {out}");
        return ExitCode::FAILURE;
    }

    let mut session = match TuiSession::start(&tui_bin, &socket) {
        Some(session) => session,
        None => {
            println!("[FAIL] tui: could not start TUI under a pty");
            return ExitCode::FAILURE;
        }
    };

    // A poll cycle + a frame — and long enough for the states to **settle**
    // (T-0079). The quiet pane turns `idle` two seconds after its last output,
    // measured from the poll that saw it, so a frame read at three seconds
    // catches it mid-transition: sometimes `working`, sometimes `idle`, and a
    // check that asserts a state cannot be built on a coin flip. Five seconds is
    // past the last transition the fixture makes; nothing here is racing.
    std::thread::sleep(Duration::from_secs(5));
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
    // color at all — and the two are adjacent, not merely both on screen. All
    // three states the fixture produces are asserted (T-0079): a dot shape that
    // only appeared for one state would be a legend, not a language.
    // The rows actually rendered, for the failure message: a check that says
    // only "missing" cannot be told from a fixture that never produced the
    // state, and this one has to distinguish "the UI lost a dot" from "the
    // engine classified the pane differently".
    let dot_rows: String = screen
        .lines()
        .filter(|line| {
            ['◉', '●', '○', '⬢', '✓']
                .iter()
                .any(|dot| line.contains(*dot))
        })
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(" | ");
    check(
        "state dots rendered",
        ['◉', '●', '○'].iter().all(|dot| screen.contains(*dot)),
        &format!("state dots missing; state rows on screen: {dot_rows:?}"),
    );
    check(
        "every state row is a dot shape and its own name",
        screen.contains("◉ question") && screen.contains("● working") && screen.contains("○ idle"),
        &format!("a state row carries only a hue; state rows on screen: {dot_rows:?}"),
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
    // Layout at 120x30 (T-0079): row 1 = question header, 2 = beta, 3 = working
    // header, 4 = gamma, 5 = idle header, 6 = alpha. gamma is the working pane,
    // so it is the row this clicks.
    session.send("\u{1b}[<0;6;4M");
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

    // ---- T-0074: the TUI manages the fleet, locally ----------------------
    //
    // The sections above read and attached; this one *drives* — keys only
    // (`s` spawn, `i` send, `x` kill), with the CLI's own sentences asserted
    // from the raw transcript. A status line is transient by design here (the
    // 1 Hz pane poll may overwrite it within a second), so asserting that a
    // sentence was *rendered* reads the bytes the TUI wrote rather than a frame
    // that may have moved on — that is what makes the checks deterministic.
    let evidence_dir_74 = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join(".loop")
        .join("evidence")
        .join("T-0074");
    if evidence {
        let _ = std::fs::create_dir_all(&evidence_dir_74);
    }
    let marker = format!("TUI-SENT-{}", std::process::id());

    // `s`: the spawn prompt is the CLI's own argument shape, quotes included.
    session.send("s");
    std::thread::sleep(Duration::from_millis(500));
    let spawn_prompt = session.screen();
    if evidence {
        let _ = std::fs::write(evidence_dir_74.join("00-spawn-prompt.txt"), &spawn_prompt);
    }
    check(
        "s opens the spawn prompt as <id> <program> [args…]",
        spawn_prompt.contains("spawn · <id> <program> [args…]"),
        "no spawn prompt on screen",
    );
    session.send("delta /bin/sh -c 'cat'");
    std::thread::sleep(Duration::from_millis(400));
    session.send("\r");
    std::thread::sleep(Duration::from_secs(2));
    let spawned = session.screen();
    if evidence {
        let _ = std::fs::write(evidence_dir_74.join("01-spawn.txt"), &spawned);
    }
    check(
        "s spawns the pane and the sidebar gains it",
        spawned.contains("delta"),
        "delta missing from the sidebar after the spawn prompt",
    );
    // The sentence is rendered via the status bar, so it is read off the
    // *frame*: ratatui's diff renderer may split a line across cursor moves in
    // the byte stream, and the reconstructed frame is the honest record of
    // what was on screen.
    check(
        "the status carried the CLI's own line (spawned delta)",
        spawned.contains("spawned delta"),
        "spawned delta never reached the status line",
    );
    // A refused spawn names why: the id is taken by the pane just spawned.
    session.send("s");
    std::thread::sleep(Duration::from_millis(400));
    session.send("delta /bin/sh -c 'cat'");
    std::thread::sleep(Duration::from_millis(400));
    session.send("\r");
    std::thread::sleep(Duration::from_secs(1));
    let refused = session.screen();
    check(
        "a refused spawn names why (pane \"delta\" already exists)",
        refused.contains("spawn: pane \"delta\" already exists"),
        "the refusal line never reached the status",
    );

    // Focus delta by clicking its sidebar row — looked up on the rendered frame
    // rather than assumed, because earlier sections spawn panes whose group
    // order the click depends on.
    let screen = session.screen();
    let edge = sidebar_column(&screen);
    let delta_row = screen
        .lines()
        .enumerate()
        .find(|(_, line)| {
            edge.is_some_and(|e| {
                line.chars()
                    .take(e as usize)
                    .collect::<String>()
                    .contains("delta")
            })
        })
        .map(|(row, _)| row);
    let Some(delta_row) = delta_row else {
        check(
            "delta's sidebar row is found for the click",
            false,
            "no delta row on screen",
        );
        drop(session);
        drop(server);
        let _ = std::fs::remove_file(&socket);
        return ExitCode::FAILURE;
    };
    session.send(&format!("\u{1b}[<0;1;{}M", delta_row + 1));
    std::thread::sleep(Duration::from_secs(1));
    check(
        "clicking delta attaches it",
        session.screen().contains("delta ["),
        "delta did not take the focused pane",
    );

    // `i`: send to the attached pane, audited as this device.
    session.send("i");
    std::thread::sleep(Duration::from_millis(400));
    session.send(&marker);
    std::thread::sleep(Duration::from_millis(300));
    session.send("\r");
    std::thread::sleep(Duration::from_secs(2));
    let sent = session.screen();
    if evidence {
        let _ = std::fs::write(evidence_dir_74.join("02-send.txt"), &sent);
    }
    check(
        "i sends the marker to the attached pane and the pane echoes it",
        sent.contains(&marker),
        &format!("the marker {marker:?} is not in the pane view"),
    );
    check(
        "the send's answer was the CLI's own line (sent to delta)",
        sent.contains("sent to delta"),
        "sent to delta never reached the status",
    );

    // `x`: kill, behind a confirmation that names the pane.
    session.send("x");
    std::thread::sleep(Duration::from_millis(400));
    let confirm = session.screen();
    if evidence {
        let _ = std::fs::write(evidence_dir_74.join("02b-kill-confirm.txt"), &confirm);
    }
    check(
        "x opens a confirmation that names the pane (kill pane delta)",
        confirm.contains("kill pane delta"),
        "the kill confirmation did not name delta",
    );
    session.send("y");
    std::thread::sleep(Duration::from_secs(2));
    let killed = session.screen();
    if evidence {
        let _ = std::fs::write(evidence_dir_74.join("03-kill.txt"), &killed);
    }
    // The status bar spans the terminal's full width, so the "killed delta"
    // sentence it carries lives inside the sidebar *region* too; the sidebar
    // itself is everything above the status row.
    let killed_body: Vec<&str> = killed.lines().collect();
    let killed_body = killed_body[..killed_body.len().saturating_sub(1)].join("\n");
    check(
        "killed delta left the sidebar",
        !sidebar_region(&killed_body).contains("delta"),
        "delta still in the sidebar after the kill",
    );
    check(
        "the kill's answer was the CLI's own line (killed delta)",
        killed.contains("killed delta"),
        "killed delta never reached the status",
    );

    // The dead-pane refusal: `i` still targets the pane that was just killed
    // (the selection is stale), and the daemon answers with the honest line.
    session.send("i");
    std::thread::sleep(Duration::from_millis(400));
    session.send("still-alive?");
    std::thread::sleep(Duration::from_millis(300));
    session.send("\r");
    std::thread::sleep(Duration::from_secs(1));
    if evidence {
        let _ = std::fs::write(
            evidence_dir_74.join("04-dead-pane-refusal.txt"),
            session.screen(),
        );
    }
    let dead = session.screen();
    check(
        "sending to a dead pane says so (send: pane \"delta\" not found)",
        dead.contains("send: pane \"delta\" not found"),
        "the dead-pane refusal never reached the status",
    );

    // Steady state: the app must repaint cells, not the screen. A frame is
    // only what changed, so idle output stays tiny and never clears.
    let before = session.transcript().len();
    std::thread::sleep(Duration::from_secs(3));
    let during = session.transcript();
    let idle_bytes = during.len() - before;
    // Said out loud on every run, not only when it fails: "no regression in the
    // steady state" is a number a reviewer should be able to read, and a budget
    // that only speaks when breached cannot be compared across changes.
    println!(
        "[INFO] tui: idle delta: {idle_bytes} bytes over 3 s (budget < 4096, no clear-screen)"
    );
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

    // ---- T-0073(a): the default is unchanged ------------------------------
    //
    // The TUI is a client, not an owner (T-0015): `q` quit it and the daemon
    // this whole slice has been driving is still serving. Both halves are in
    // this check on purpose — the TUI must be *gone* (a prompt holding it open
    // is not a quit) and the daemon must answer. It is the check the opt-in's
    // default turns red if it ever flips: nothing here passes
    // `--shutdown-on-exit`.
    let (serving, panes_out) = cli(&cli_bin, &socket, &["panes"]);
    let daemon_pid = daemon_pid_naming(&socket);
    let quit_clean = session.exited();
    check(
        "T-0073(a): a default quit leaves the daemon serving",
        quit_clean && serving && daemon_pid.is_some(),
        &format!("TUI exited: {quit_clean}, daemon answers: {serving}, daemon pid: {daemon_pid:?}"),
    );
    let _ = std::fs::write(
        evidence_dir_73.join("01-default-quit-daemon-serving.txt"),
        format!(
            "T-0073(a) — `q` with no opt-in: the TUI is gone, the daemon is not.\n\
             \n\
             socket {} exists (a serving daemon holds it)\n\
             daemon pid (by /proc argv scan): {daemon_pid:?}\n\
             `arreo panes --socket {}` -> exit_ok={serving}:\n{panes_out}\n",
            socket.display(),
            socket.display(),
        ),
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

    // ---- T-0079: the 30-pane wall's first frame --------------------------
    //
    // The budget row this asserts is **read from `perf-budget.toml`**, not
    // copied: the file is the law, and a slice carrying its own constant is a
    // second source for one fact (the same rule the `reattach` slice follows).
    //
    // The fixture is the shape that *fails today*: 30 panes each emitting a
    // marker every 500 ms, so every one of them is `working` — in neither of
    // the two states the old poller waited for per pane. That is the whole
    // point: the old poll path paid two 150 ms blocking `Wait` timeouts per
    // pane, so 30 working panes could not paint at all for ~9-10 s (the
    // measured baseline is 10.3 s; see `.loop/evidence/T-0079/`). The panes are
    // meant to be busy — "make the panes idle so the wall is fast" would be
    // passing for the wrong reason.
    let wall_budget_ms = crate::bench::budget_target("tui_attach_30panes_ms");
    let wall_socket =
        std::env::temp_dir().join(format!("arreo-e2e-wall-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&wall_socket);
    let wall_server = match TestServer::spawn(&server_bin, &wall_socket, "30-pane server start") {
        Ok(server) => server,
        Err(code) => return code,
    };
    wait_bound(&wall_socket);

    /// The number of panes in the wall. A serial pass cannot pass this: the
    /// old path paid 30 × (2 × 150 ms) of blocking waits, so 30 is more than
    /// enough for the measurement to fail on the old code and pass on the
    /// batched one — one pane would prove nothing, because one `Wait` pair
    /// (300 ms) is already near the budget and a single pane cannot show that
    /// the cost is per-pane. 30 is also the size the budget row names.
    const WALL_PANES: usize = 30;

    let wall_ids: Vec<String> = (0..WALL_PANES).map(|i| format!("wall-{i:02}")).collect();
    for (i, id) in wall_ids.iter().enumerate() {
        // Bounded (`i < 240` ≈ 2 minutes) as well as explicitly killed below:
        // a test must not leave a process behind, and an unbounded `while :`
        // would outlive the slice if the daemon were killed before the panes.
        let body = format!(
            "i=0; while [ $i -lt 240 ]; do echo \"W{i:02}-$i\"; i=$((i+1)); sleep 0.5; done"
        );
        let (ok, out) = cli(
            &cli_bin,
            &wall_socket,
            &["spawn", id, "/bin/sh", "-c", &body],
        );
        if !ok {
            check(
                "the 30-pane fixture spawns",
                false,
                &format!("spawn {id}: {out}"),
            );
            drop(wall_server);
            let _ = std::fs::remove_file(&wall_socket);
            return ExitCode::FAILURE;
        }
    }

    // The daemon's own answer over the verb the TUI now uses — the state of
    // every pane, told in one reply. Probing it directly is what makes the
    // fixture's *shape* an assertion rather than an assumption: if a pane were
    // in `question` or `blocked`, a `Wait` would have resolved it without
    // timing out, and the measurement would not be measuring the failing case.
    let mut told: Vec<(String, Option<AgentState>)> = Vec::new();
    let mut probe_ms: Option<u128> = None;
    let probe_deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < probe_deadline {
        let attempt = Instant::now();
        match probe_pane_states(&wall_socket) {
            Some(panes) if panes.len() == WALL_PANES => {
                told = panes;
                probe_ms = Some(attempt.elapsed().as_millis());
                break;
            }
            _ => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    check(
        "the 30-pane fixture is up",
        told.len() == WALL_PANES,
        &format!("{} panes answered", told.len()),
    );
    let waited_states: Vec<String> = told
        .iter()
        .filter(|(_, state)| {
            matches!(
                state,
                Some(AgentState::Question) | Some(AgentState::Blocked)
            )
        })
        .map(|(_, state)| format!("{state:?}"))
        .collect();
    check(
        "no wall pane is in a state a `Wait` would have resolved",
        waited_states.is_empty(),
        &format!(
            "{} pane(s) were waiting: {waited_states:?}",
            waited_states.len()
        ),
    );
    check(
        "the daemon tells every pane's state in one reply",
        told.iter().all(|(_, state)| state.is_some()),
        "a pane came back with no state, so the reply is not the detailed shape",
    );
    {
        // Written on every run, not only under `--interactive-evidence`: the
        // number this task exists to assert is evidence a reviewer reads, and a
        // measurement nobody can see afterwards is a claim.
        let mut lines = String::from("pane\tstate\n");
        for (id, state) in &told {
            lines.push_str(&format!("{id}\t{state:?}\n"));
        }
        lines.push_str(&format!(
            "\n# one `PanesDetail` round trip (incl. the handshake): {} ms\n",
            probe_ms.map_or_else(|| "n/a".to_string(), |ms| ms.to_string())
        ));
        let _ = std::fs::write(evidence_dir_79.join("fixture-states.tsv"), lines);
    }

    // **The measurement.** From the moment the TUI process exists to the first
    // frame whose sidebar carries every pane id — i.e. the first frame that is
    // *correct*, which is the thing the old code could not produce inside four
    // seconds.
    //
    // The pty is resized to 60×160 before the frame is read, because 30 pane
    // rows plus their state-group headers do not fit a 30-row terminal: the
    // measurement is of the poll pass, and a taller pty is the operator's own
    // terminal, not a favour to the code under test. The resize lands in the
    // same event loop tick as the startup (the loop polls input every 50 ms),
    // so it does not hand the poller a head start.
    let wall_started = Instant::now();
    let Some(mut wall) = TuiSession::start(&tui_bin, &wall_socket) else {
        check("the 30-pane TUI session starts", false, "no pty");
        drop(wall_server);
        let _ = std::fs::remove_file(&wall_socket);
        return ExitCode::FAILURE;
    };
    wall.resize(60, 160);
    let mut first_frame_ms: Option<u128> = None;
    let mut wall_frame = String::new();
    let frame_deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < frame_deadline {
        let screen = wall.screen();
        if wall_ids.iter().all(|id| screen.contains(id.as_str())) {
            first_frame_ms = Some(wall_started.elapsed().as_millis());
            wall_frame = screen;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = std::fs::write(evidence_dir_79.join("first-frame-30panes.txt"), &wall_frame);
    check(
        "all 30 panes are on the first painted frame",
        first_frame_ms.is_some(),
        &format!(
            "only {} of {WALL_PANES} pane ids were on screen within 20 s",
            wall_ids
                .iter()
                .filter(|id| wall_frame.contains(id.as_str()))
                .count()
        ),
    );
    // RAM for every pane, which the batched reply carries (`ram_kb`) and the
    // old path fetched with one `MetricsReq` round-trip per pane. A frame with
    // 30 readings is evidence the one-pass computation really did the work.
    let ram_readings = wall_frame
        .split_whitespace()
        .filter(|word| {
            word.len() > 1
                && (word.ends_with('K') || word.ends_with('M'))
                && word[..word.len() - 1].parse::<u64>().is_ok()
        })
        .count();
    check(
        "the one reply carries RAM for every pane",
        ram_readings >= WALL_PANES,
        &format!("{ram_readings} RAM readings on the frame"),
    );

    let measured_ms = first_frame_ms.unwrap_or(u128::MAX);
    let budget_ok = match &wall_budget_ms {
        Ok(target) => measured_ms <= u128::from(*target),
        Err(_) => false,
    };
    check(
        "the 30-pane first frame is inside tui_attach_30panes_ms (read from perf-budget.toml)",
        budget_ok,
        &match &wall_budget_ms {
            Ok(target) => format!(
                "first correct frame in {measured_ms} ms, budget {target} ms (perf-budget.toml)"
            ),
            Err(e) => e.clone(),
        },
    );
    println!(
        "[INFO] tui: 30-pane first frame: {measured_ms} ms vs {} ms (perf-budget.toml \
         tui_attach_30panes_ms)",
        wall_budget_ms
            .as_ref()
            .map(u64::to_string)
            .unwrap_or_else(|_| "?".to_string())
    );
    {
        let _ = std::fs::write(
            evidence_dir_79.join("measurement.txt"),
            format!(
                "tui_attach_30panes_ms (perf-budget.toml): {}\n\
                 measured first correct frame: {measured_ms} ms\n\
                 panes: {WALL_PANES} (each emitting a marker every 500 ms => working)\n\
                 one `PanesDetail` round trip (incl. handshake): {} ms\n\
                 pty: 60 rows x 160 cols\n\
                 pass: {budget_ok}\n\
                 \n\
                 Measured by `cargo xtask e2e --slice tui`: a real daemon holding\n\
                 30 real panes, the real TUI on a real pty, timed from the moment\n\
                 the TUI process exists to the first frame whose sidebar carries\n\
                 every pane id. The budget number is read from perf-budget.toml.\n",
                wall_budget_ms
                    .as_ref()
                    .map(u64::to_string)
                    .unwrap_or_else(|_| "?".to_string()),
                probe_ms.map_or_else(|| "n/a".to_string(), |ms| ms.to_string()),
            ),
        );
    }

    // Kill every pane before the daemon goes, so no `sh` survives the slice,
    // then quit the TUI cleanly.
    //
    // Over the protocol, and checked: `arreo` has no `kill` subcommand, so the
    // old `cli(..., ["kill", id])` here was a silent no-op that left 30 panes
    // running past the end of the run. A cleanup whose failure is swallowed is
    // not a cleanup.
    let mut killed = 0usize;
    for id in &wall_ids {
        if kill_pane(&wall_socket, id) {
            killed += 1;
        }
    }
    check(
        "the wall's panes are killed before their daemon goes",
        killed == wall_ids.len(),
        &format!("killed {killed} of {} wall panes", wall_ids.len()),
    );
    wall.send("q");
    std::thread::sleep(Duration::from_millis(500));
    drop(wall);
    drop(wall_server);
    for path in sidecars(&wall_socket) {
        let _ = std::fs::remove_file(path);
    }

    // The main fixture's own panes (alpha, beta, gamma): the same rule, and the
    // daemon is still up to take the kill.
    for (id, _) in &panes {
        let _ = kill_pane(&socket, id);
    }

    drop(session);
    drop(server);
    for path in sidecars(&socket) {
        let _ = std::fs::remove_file(path);
    }

    // ---- T-0073: the opt-in exit -------------------------------------------
    //
    // The default is checked above, on the daemon this whole slice has been
    // driving. What is left is the opt-in itself, each case on its **own**
    // daemon (the good cases end with the daemon gone, so they cannot share
    // one): the flag, the config key, the override that beats it, the
    // confirmation with a live pane, `--yes`, and the refusal on a target that
    // is not this machine.
    let config_73 =
        std::env::temp_dir().join(format!("arreo-e2e-tui73-{}.toml", std::process::id()));
    std::fs::write(&config_73, "[tui]\nexit_kills_daemon = true\n")
        .expect("write the T-0073 config");
    let config_arg_73 = config_73.to_str().expect("utf-8 temp path").to_string();

    // (b) `--shutdown-on-exit`, nothing live: no prompt, a clean stop.
    if let Some(case) = Daemon::start(&server_bin, &cli_bin, "b", &[]) {
        if let Some(mut tui) =
            TuiSession::start_with(&tui_bin, &case.socket, &["--shutdown-on-exit"], &[])
        {
            std::thread::sleep(Duration::from_secs(3));
            let _ = std::fs::write(evidence_dir_73.join("10-b-before-quit.txt"), tui.screen());
            tui.send("q");
            let stopped = wait_exit(&mut tui, Duration::from_secs(20));
            let transcript = String::from_utf8_lossy(&tui.transcript()).to_string();
            check(
                "T-0073(b): --shutdown-on-exit quits with nothing live (no prompt)",
                stopped,
                "the TUI never exited after q",
            );
            // T-0012's contract: the daemon's last act is removing the socket
            // file, and the process is gone with it. Both, because a socket
            // left behind would be the "unlink while serving" failure read the
            // other way round.
            let socket_gone = !case.socket.exists();
            let process_gone = wait_daemon_gone(&case.socket, Duration::from_secs(5));
            check(
                "T-0073(b): the daemon is gone (process + socket file, per T-0012)",
                socket_gone && process_gone,
                &format!(
                    "socket exists: {}, process: {}",
                    !socket_gone, !process_gone
                ),
            );
            check(
                "T-0073(b): nothing answers on the socket afterwards",
                !case.serving(&cli_bin),
                "the daemon still answers after the stop",
            );
            check(
                "T-0073(b): the stop was the CLI's own line (server stopped)",
                transcript.contains("server stopped (pid"),
                "the CLI's stop line never reached the terminal",
            );
            let _ = std::fs::write(
                evidence_dir_73.join("11-b-after-stop.txt"),
                format!(
                    "T-0073(b) — `arreo-tui --shutdown-on-exit`, no panes, `q`.\n\
                     \n\
                     TUI exited: {stopped}\n\
                     socket file removed (the daemon's last act, T-0012): {socket_gone}\n\
                     no daemon process names the socket: {process_gone}\n\
                     `arreo panes` afterwards: exit_ok={}\n\
                     the CLI's own stop line reached the terminal: {}\n\
                     \n\
                     --- the stop line ---\n{}\n",
                    case.serving(&cli_bin),
                    transcript.contains("server stopped (pid"),
                    transcript
                        .lines()
                        .find(|line| line.contains("server stopped"))
                        .unwrap_or("<not found>"),
                ),
            );
        }
        drop(case);
    }

    // (b2) the same opt-in, from the config the daemon already reads.
    if let Some(case) = Daemon::start(&server_bin, &cli_bin, "b2", &[]) {
        if let Some(mut tui) =
            TuiSession::start_with(&tui_bin, &case.socket, &["--config", &config_arg_73], &[])
        {
            std::thread::sleep(Duration::from_secs(3));
            tui.send("q");
            let stopped = wait_exit(&mut tui, Duration::from_secs(20));
            let gone =
                !case.socket.exists() && wait_daemon_gone(&case.socket, Duration::from_secs(5));
            check(
                "T-0073(b2): tui.exit_kills_daemon = true stops the daemon too",
                stopped && gone,
                &format!("exited: {stopped}, daemon gone: {gone}"),
            );
            let _ = std::fs::write(
                evidence_dir_73.join("12-b2-config-quit.txt"),
                format!(
                    "T-0073(b2) — the config key, no flag: [tui] exit_kills_daemon = true.\n\
                     TUI exited: {stopped}; daemon gone: {gone}\n"
                ),
            );
        }
        drop(case);
    }

    // (b3) `--no-shutdown-on-exit` overrules a config that says true.
    if let Some(case) = Daemon::start(&server_bin, &cli_bin, "b3", &[]) {
        if let Some(mut tui) = TuiSession::start_with(
            &tui_bin,
            &case.socket,
            &["--config", &config_arg_73, "--no-shutdown-on-exit"],
            &[],
        ) {
            std::thread::sleep(Duration::from_secs(3));
            tui.send("q");
            let stopped = wait_exit(&mut tui, Duration::from_secs(20));
            let still_serving = case.serving(&cli_bin);
            check(
                "T-0073(b3): --no-shutdown-on-exit overrules the config for the run",
                stopped && still_serving,
                &format!("exited: {stopped}, daemon still serving: {still_serving}"),
            );
            let _ = std::fs::write(
                evidence_dir_73.join("13-b3-no-shutdown-override.txt"),
                format!(
                    "T-0073(b3) — the flag over the file: --no-shutdown-on-exit with\n\
                     tui.exit_kills_daemon = true. TUI exited: {stopped};\n\
                     `arreo panes` afterwards: exit_ok={still_serving} (the daemon is up)\n"
                ),
            );
        }
        drop(case);
    }

    // (c) a live pane: the confirmation asks first, and `n` aborts.
    if let Some(case) = Daemon::start(&server_bin, &cli_bin, "c", &[("zeta", "sleep 60")]) {
        if let Some(mut tui) =
            TuiSession::start_with(&tui_bin, &case.socket, &["--shutdown-on-exit"], &[])
        {
            // Long enough for the sidebar to have zeta *and* for its state to
            // settle, so the guard is reading a live pane rather than racing
            // the first poll.
            std::thread::sleep(Duration::from_secs(4));
            tui.send("q");
            std::thread::sleep(Duration::from_millis(600));
            let prompt = tui.screen();
            let _ = std::fs::write(evidence_dir_73.join("14-c-confirm.txt"), &prompt);
            check(
                "T-0073(c): a flag quit with a live pane asks first, naming what dies",
                prompt.contains("stop this machine's daemon on exit?")
                    && prompt.contains("live panes (1): zeta"),
                "the confirmation did not name the live pane",
            );
            tui.send("n");
            std::thread::sleep(Duration::from_millis(600));
            let after = tui.screen();
            let _ = std::fs::write(evidence_dir_73.join("15-c-after-n.txt"), &after);
            check(
                "T-0073(c): `n` aborts the quit — the TUI is still up",
                !tui.exited(),
                "the TUI exited despite the declined confirmation",
            );
            check(
                "T-0073(c): ...and the daemon is untouched",
                case.serving(&cli_bin) && case.socket.exists(),
                "the daemon stopped after a declined confirmation",
            );
            check(
                "T-0073(c): ...with the CLI's own refusal line on the status",
                after.contains("shutdown-on-exit: not confirmed; nothing changed"),
                "the declined confirmation said nothing",
            );
            // The same opt-in still works after the abort: kill the pane (so no
            // `sh` outlives the case), wait for the sidebar to drop it, then
            // quit again — nothing live now, so no prompt and a clean stop.
            check(
                "T-0073(c): the pane can be killed over the protocol",
                kill_pane(&case.socket, "zeta"),
                "the kill verb refused",
            );
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline && sidebar_region(&tui.screen()).contains("zeta") {
                std::thread::sleep(Duration::from_millis(200));
            }
            tui.send("q");
            let stopped = wait_exit(&mut tui, Duration::from_secs(20));
            let gone =
                !case.socket.exists() && wait_daemon_gone(&case.socket, Duration::from_secs(5));
            check(
                "T-0073(c): after the abort the same opt-in still stops the daemon",
                stopped && gone,
                &format!("exited: {stopped}, daemon gone: {gone}"),
            );
            let _ = std::fs::write(
                evidence_dir_73.join("16-c-after-abort-stop.txt"),
                format!(
                    "T-0073(c) — declined once, then quit again with nothing live.\n\
                     TUI exited: {stopped}; daemon gone: {gone}\n"
                ),
            );
        }
        drop(case);
    }

    // (c2) a live pane, confirmed: `y` is what stops it.
    if let Some(case) = Daemon::start(&server_bin, &cli_bin, "c2", &[("omega", "sleep 20")]) {
        if let Some(mut tui) =
            TuiSession::start_with(&tui_bin, &case.socket, &["--shutdown-on-exit"], &[])
        {
            std::thread::sleep(Duration::from_secs(4));
            tui.send("q");
            std::thread::sleep(Duration::from_millis(600));
            let prompt = tui.screen();
            let _ = std::fs::write(evidence_dir_73.join("17-c2-confirm.txt"), &prompt);
            check(
                "T-0073(c2): the confirmation is what stands between q and the stop",
                prompt.contains("stop this machine's daemon on exit?")
                    && prompt.contains("live panes (1): omega"),
                "the confirmation did not name omega",
            );
            tui.send("y");
            let stopped = wait_exit(&mut tui, Duration::from_secs(20));
            let gone =
                !case.socket.exists() && wait_daemon_gone(&case.socket, Duration::from_secs(5));
            check(
                "T-0073(c2): `y` confirms, and the daemon stops with the pane live",
                stopped && gone,
                &format!("exited: {stopped}, daemon gone: {gone}"),
            );
            // The pane's child outlives the daemon by design ("children keep
            // running") and self-terminates: `sleep 20`, so the case cannot
            // leave a stray behind for longer than that.
            let _ = std::fs::write(
                evidence_dir_73.join("18-c2-after-stop.txt"),
                format!(
                    "T-0073(c2) — confirmed with omega live: TUI exited: {stopped};\n\
                     daemon gone: {gone}. omega's `sleep 20` keeps running (the daemon\n\
                     says so itself) and exits on its own.\n"
                ),
            );
        }
        drop(case);
    }

    // (d) `--yes`: the scripted path skips the question. A live pane is the
    // hard case for it, and the proof that no prompt appeared is that `q`
    // *completed* — a prompt holds the loop open until it is answered.
    if let Some(case) = Daemon::start(&server_bin, &cli_bin, "d", &[("sigma", "sleep 20")]) {
        if let Some(mut tui) = TuiSession::start_with(
            &tui_bin,
            &case.socket,
            &["--shutdown-on-exit", "--yes"],
            &[],
        ) {
            std::thread::sleep(Duration::from_secs(4));
            let _ = std::fs::write(evidence_dir_73.join("19-d-before-quit.txt"), tui.screen());
            tui.send("q");
            let stopped = wait_exit(&mut tui, Duration::from_secs(20));
            let gone =
                !case.socket.exists() && wait_daemon_gone(&case.socket, Duration::from_secs(5));
            check(
                "T-0073(d): --yes stops without asking, live pane or not (scripts)",
                stopped && gone,
                &format!("exited: {stopped}, daemon gone: {gone}"),
            );
        }
        drop(case);
    }

    // The refusal: the flag names *this machine's* daemon, and a TUI attached
    // to another machine must not stop it. Run without a pty on purpose — the
    // refusal happens before the terminal is touched, which is what makes it a
    // startup fact rather than a screen the operator has to read.
    let refusal_cases: [(&str, &[&str], &str); 3] = [
        (
            "flag",
            &[
                "--remote",
                "127.0.0.1:1",
                "--peer",
                "deadbeef",
                "--shutdown-on-exit",
            ],
            "--shutdown-on-exit",
        ),
        (
            "config",
            &["--machine", "build", "--config", &config_arg_73],
            "tui.exit_kills_daemon = true",
        ),
        (
            "yes",
            &["--socket", "/tmp/arreo-tui-73-nothing.sock", "--yes"],
            "--yes",
        ),
    ];
    let mut refusal_text = String::new();
    for (name, args, needle) in refusal_cases {
        let output = std::process::Command::new(&tui_bin)
            .args(args)
            .output()
            .expect("run arreo-tui");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let code = output.status.code();
        check(
            &format!("T-0073: {name}: the refusal is loud and distinct (exit 2)"),
            code == Some(2) && text.contains(needle),
            &format!("code {code:?}: {text}"),
        );
        refusal_text.push_str(&format!(
            "$ arreo-tui {}\nexit {code:?}\n{text}\n\n",
            args.join(" ")
        ));
    }
    check(
        "T-0073: the remote refusal says what it refuses",
        refusal_text.contains("must not stop that machine's daemon"),
        "the refusal did not say it refuses a remote daemon",
    );
    let _ = std::fs::write(evidence_dir_73.join("20-refusals.txt"), &refusal_text);
    let _ = std::fs::remove_file(&config_73);

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

/// Ask a daemon for every pane's derived state, over the protocol by hand.
///
/// A direct probe rather than a library client, because the point is to read
/// *the reply the TUI reads* — `PanesDetail` — and to show what is in it: one
/// reply, every pane's state. Synchronous, bounded (2 s reads, one connection,
/// two verbs), and closed on the way out.
fn probe_pane_states(socket: &std::path::Path) -> Option<Vec<(String, Option<AgentState>)>> {
    use arreo_core::proto::{client_versions, Message, VERSION};

    let mut stream = std::os::unix::net::UnixStream::connect(socket).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    write_message(
        &mut stream,
        &Message::Hello {
            v: VERSION,
            client: "xtask-tui-slice".to_string(),
            wants: client_versions(),
        },
    )?;
    match read_message(&mut stream)? {
        Message::Welcome { .. } => {}
        _ => return None,
    }
    write_message(
        &mut stream,
        &Message::PanesDetail {
            v: VERSION,
            panes: Vec::new(),
        },
    )?;
    match read_message(&mut stream)? {
        Message::PanesDetail { panes, .. } => Some(
            panes
                .into_iter()
                .map(|pane| (pane.id, Some(pane.state)))
                .collect(),
        ),
        _ => None,
    }
}

/// Write one length-prefixed frame to a socket.
fn write_message(
    stream: &mut std::os::unix::net::UnixStream,
    message: &arreo_core::proto::Message,
) -> Option<()> {
    use std::io::Write;
    let frame = arreo_core::proto::codec::encode_frame(message).ok()?;
    stream.write_all(&frame).ok()?;
    stream.flush().ok()?;
    Some(())
}

/// Read exactly one length-prefixed frame from a socket.
fn read_message(stream: &mut std::os::unix::net::UnixStream) -> Option<arreo_core::proto::Message> {
    use std::io::Read;
    // The framing the codec writes: `u32 LE` body length, then the body.
    let mut len = [0u8; 4];
    stream.read_exact(&mut len).ok()?;
    let mut body = vec![0u8; u32::from_le_bytes(len) as usize];
    stream.read_exact(&mut body).ok()?;
    arreo_core::proto::codec::decode(&body).ok()
}

/// Kill a pane over the protocol.
///
/// There is no `arreo kill` subcommand — the verb is on the wire (`Message::Kill`,
/// which is what the TUI's own `x` sends) and the CLI never grew a front door for
/// it. A slice that wants a pane gone must speak the protocol, and it must check
/// the answer: a cleanup that silently fails is how panes outlive the run.
fn kill_pane(socket: &std::path::Path, id: &str) -> bool {
    use arreo_core::proto::{client_versions, Message, VERSION};

    let Some(mut stream) = std::os::unix::net::UnixStream::connect(socket).ok() else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    if write_message(
        &mut stream,
        &Message::Hello {
            v: VERSION,
            client: "xtask-tui-slice".to_string(),
            wants: client_versions(),
        },
    )
    .is_none()
    {
        return false;
    }
    if !matches!(read_message(&mut stream), Some(Message::Welcome { .. })) {
        return false;
    }
    if write_message(
        &mut stream,
        &Message::Kill {
            v: VERSION,
            id: id.to_string(),
        },
    )
    .is_none()
    {
        return false;
    }
    matches!(read_message(&mut stream), Some(Message::Ok { .. }))
}

/// One fresh daemon for a T-0073 case, on its own socket, with the panes the
/// case asked for.
///
/// Its own daemon because the good cases *end* with the daemon gone: sharing
/// one would make every later case depend on the earlier ones' outcome. Drop
/// removes the socket and its store, so a case cannot leak a file into the
/// next.
struct Daemon {
    socket: PathBuf,
    /// The RAII handle, held for the case's whole life and never read: its
    /// `Drop` is the kill + reap, and `Daemon::drop` (which runs first) removes
    /// the files.
    _server: TestServer,
}

impl Daemon {
    /// Start a daemon and spawn `panes` (`id`, shell body) into it. `None` (with
    /// the reason printed) when either step fails — the caller's checks then
    /// never run, which is a failure the slice reports as such rather than a
    /// silent pass.
    fn start(
        server_bin: &std::path::Path,
        cli_bin: &std::path::Path,
        tag: &str,
        panes: &[(&str, &str)],
    ) -> Option<Self> {
        let socket =
            std::env::temp_dir().join(format!("arreo-e2e-tui73-{tag}-{}.sock", std::process::id()));
        for path in sidecars(&socket) {
            let _ = std::fs::remove_file(path);
        }
        let server =
            TestServer::spawn(server_bin, &socket, &format!("T-0073 {tag} server")).ok()?;
        wait_bound(&socket);
        for (id, body) in panes {
            let (ok, out) = cli(cli_bin, &socket, &["spawn", id, "/bin/sh", "-c", body]);
            if !ok {
                println!("[FAIL] tui: T-0073 {tag}: spawn {id}: {out}");
                return None;
            }
        }
        Some(Self {
            socket,
            _server: server,
        })
    }

    /// Whether this daemon is still serving: the socket answers **and** a verb
    /// comes back. A connect alone would pass on a socket nobody drains, which
    /// is exactly the state "unlinked while serving" would leave.
    fn serving(&self, cli_bin: &std::path::Path) -> bool {
        std::os::unix::net::UnixStream::connect(&self.socket).is_ok()
            && cli(cli_bin, &self.socket, &["panes"]).0
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // `server` drops after this body (fields drop last), so the daemon is
        // killed and reaped before the files go.
        for path in sidecars(&self.socket) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// `<socket>` and the files the daemon keeps beside it.
fn sidecars(socket: &std::path::Path) -> Vec<PathBuf> {
    ["", ".db", ".db-wal", ".db-shm", ".lock"]
        .iter()
        .map(|suffix| {
            let mut path = socket.as_os_str().to_os_string();
            path.push(suffix);
            PathBuf::from(path)
        })
        .collect()
}

/// The pid of the daemon serving `socket`, by the same /proc scan `arreo server
/// stop` resolves its pid from: a process whose argv names `arreo-server` and
/// this socket. `None` = no such process (a zombie has an empty cmdline, so it
/// reads as gone — which is what "not serving" means).
fn daemon_pid_naming(socket: &std::path::Path) -> Option<u32> {
    let want = socket.to_string_lossy().to_string();
    for entry in std::fs::read_dir("/proc").ok()?.filter_map(|e| e.ok()) {
        // `continue`, never `?`: /proc holds non-numeric entries and
        // unreadable pids, and either must skip rather than end the scan.
        let pid: u32 = match entry.file_name().to_string_lossy().parse() {
            Ok(pid) => pid,
            Err(_) => continue,
        };
        let cmdline = match std::fs::read(format!("/proc/{pid}/cmdline")) {
            Ok(cmdline) => cmdline,
            Err(_) => continue,
        };
        let parts: Vec<&str> = cmdline
            .split(|b| *b == 0)
            .filter_map(|s| std::str::from_utf8(s).ok())
            .collect();
        if parts.iter().any(|p| p.ends_with("arreo-server")) && parts.iter().any(|p| *p == want) {
            return Some(pid);
        }
    }
    None
}

/// Wait (bounded) for the daemon process to leave the process table.
fn wait_daemon_gone(socket: &std::path::Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if daemon_pid_naming(socket).is_none() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    daemon_pid_naming(socket).is_none()
}

/// Wait (bounded) for a pty session's process to exit. The reader thread drains
/// the master, so a chatty child cannot block on a full pipe while we wait.
fn wait_exit(tui: &mut TuiSession, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if tui.exited() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    tui.exited()
}
