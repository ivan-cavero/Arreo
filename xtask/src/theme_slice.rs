//! T-0016 theme slice: the theming engine, judged by what reaches a terminal.
//!
//! Three layers, none of them mocked:
//! 1. **Engine**: every built-in loads in both variants at every depth, and the
//!    quantizer's output is asserted byte-for-byte against the terminal shapes
//!    the roadmap names (iTerm truecolor, Windows Terminal 256, legacy 16).
//! 2. **Terminal**: the real `arreo-tui` binary runs on a pty under each shape
//!    (env-driven depth detection) and its *actual escape bytes* are checked —
//!    a truecolor SGR on a 256-color terminal is the "glitched output" bug.
//! 3. **Shared tokens**: the same theme renders the reference HTML, and the
//!    colors in that HTML are compared with the sequences the TUI emitted, so
//!    the docs and the sidebar cannot drift.
//!
//! `--interactive-evidence` writes every capture (and the HTML) to
//! `.loop/evidence/T-0016/`.

use crate::harness::{bins, cli, wait_bound, TestServer, TuiSession};
use arreo_core::theme::{reference_html, Catalog, Color, Depth, Theme, Variant};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

/// The terminal shapes the acceptance criteria name, as environment probes.
struct Shape {
    name: &'static str,
    args: &'static [&'static str],
    env: &'static [(&'static str, &'static str)],
    depth: Depth,
    /// A full-color terminal, a 256 one, or a monochrome one.
    expect_truecolor: bool,
}

const SHAPES: &[Shape] = &[
    // iTerm2 / Alacritty / kitty shape: COLORTERM=truecolor.
    Shape {
        name: "iterm-truecolor",
        args: &[],
        env: &[("COLORTERM", "truecolor"), ("TERM", "xterm-256color")],
        depth: Depth::Truecolor,
        expect_truecolor: true,
    },
    // Windows Terminal / older xterm: 256 colors, no COLORTERM hint.
    Shape {
        name: "windows-terminal-256",
        args: &[],
        env: &[("COLORTERM", ""), ("TERM", "xterm-256color")],
        depth: Depth::Ansi256,
        expect_truecolor: false,
    },
    // Legacy Terminal.app / conhost shape: bare xterm, 16 colors.
    Shape {
        name: "legacy-16color",
        args: &[],
        env: &[("COLORTERM", ""), ("TERM", "xterm")],
        depth: Depth::Ansi16,
        expect_truecolor: false,
    },
    // NO_COLOR: the engine must stop emitting color entirely.
    Shape {
        name: "no-color",
        args: &[],
        env: &[("NO_COLOR", "1"), ("COLORTERM", "truecolor")],
        depth: Depth::NoColor,
        expect_truecolor: false,
    },
];

pub fn run(rest: &[String]) -> ExitCode {
    let evidence = rest.iter().any(|a| a == "--interactive-evidence");
    let evidence_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join(".loop")
        .join("evidence")
        .join("T-0016");
    if evidence {
        let _ = std::fs::create_dir_all(&evidence_dir);
    }

    let mut failures = 0usize;
    let mut passes = 0usize;
    let mut check = |name: &str, ok: bool, detail: &str| {
        if ok {
            println!("[PASS] theme: {name}");
            passes += 1;
        } else {
            println!("[FAIL] theme: {name}: {detail}");
            failures += 1;
        }
    };

    // ---- Layer 1: the engine -------------------------------------------------
    let catalog = Catalog::builtin();
    let mut engine_ok = true;
    let mut detail = String::new();
    for name in Catalog::builtin_names() {
        for variant in [Variant::Dark, Variant::Light] {
            for depth in [
                Depth::Truecolor,
                Depth::Ansi256,
                Depth::Ansi16,
                Depth::NoColor,
            ] {
                match catalog.theme_with_depth(&name, variant, depth) {
                    Ok(theme) => {
                        // A token that is set must be renderable at this depth.
                        if depth == Depth::NoColor && theme.color("primary") != Color::None {
                            engine_ok = false;
                            detail = format!("{name} kept color under NO_COLOR");
                        }
                        if depth == Depth::Ansi256 {
                            if let Color::Rgb(..) = theme.color("primary") {
                                engine_ok = false;
                                detail = format!("{name} emitted 24-bit at 256 colors");
                            }
                        }
                    }
                    Err(e) => {
                        engine_ok = false;
                        detail = format!("{name} ({}): {e}", variant.as_str());
                    }
                }
            }
        }
    }
    check(
        "every built-in loads in both variants at every depth",
        engine_ok,
        &detail,
    );

    // Byte-level quantization fixtures per terminal shape.
    let brand = Color::Rgb(0x7d, 0xcf, 0xff);
    check(
        "truecolor fixture is a 24-bit SGR",
        brand.fg_sequence(Depth::Truecolor) == "\u{1b}[38;2;125;207;255m",
        &brand.fg_sequence(Depth::Truecolor),
    );
    let at_256 = brand.fg_sequence(Depth::Ansi256);
    check(
        "256-shape fixture never contains a 24-bit SGR",
        at_256.starts_with("\u{1b}[38;5;") && !at_256.contains("38;2;"),
        &at_256,
    );
    let at_16 = brand.fg_sequence(Depth::Ansi16);
    check(
        "legacy-16 fixture stays inside the terminal's own palette",
        at_16.starts_with("\u{1b}[38;5;")
            && sgr_params(at_16.as_bytes())
                .get(2)
                .is_some_and(|index| *index < 16)
            && !at_16.contains("38;2;"),
        &at_16,
    );
    check(
        "NO_COLOR emits no escape bytes at all",
        brand.fg_sequence(Depth::NoColor).is_empty(),
        &brand.fg_sequence(Depth::NoColor),
    );

    // ---- Layers 2 + 3: a real terminal, and the shared-token HTML -----------
    let socket = std::env::temp_dir().join(format!("arreo-e2e-theme-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&socket);
    let (server_bin, cli_bin, tui_bin) = bins();
    for bin in [&server_bin, &cli_bin, &tui_bin] {
        if !bin.exists() {
            println!(
                "[FAIL] theme: missing binary {} (build first)",
                bin.display()
            );
            return ExitCode::FAILURE;
        }
    }
    let server = match TestServer::spawn(&server_bin, &socket, "server start") {
        Ok(server) => server,
        Err(code) => return code,
    };
    wait_bound(&socket);
    let (ok, out) = cli(
        &cli_bin,
        &socket,
        &[
            "spawn",
            "alpha",
            "/bin/sh",
            "-c",
            "sleep 0.5; printf 'Proceed? [y/n] '; sleep 120",
        ],
    );
    if !ok {
        println!("[FAIL] theme: spawn alpha: {out}");
        return ExitCode::FAILURE;
    }
    // The question state is what puts a state color on screen.
    let _ = cli(
        &cli_bin,
        &socket,
        &["wait", "alpha", "--state", "question", "--timeout", "10s"],
    );
    std::thread::sleep(Duration::from_secs(1));

    // alpha is in `question`, so the sidebar paints the question token; that
    // is the byte-exact sequence each shape must (or must not) produce.
    let question_color = catalog
        .theme_with_depth("arreo", Variant::Dark, Depth::Truecolor)
        .expect("arreo loads")
        .color("question");

    for shape in SHAPES {
        let Some(session) = TuiSession::start_with(&tui_bin, &socket, shape.args, shape.env) else {
            println!("[FAIL] theme: could not start the TUI for {}", shape.name);
            return ExitCode::FAILURE;
        };
        std::thread::sleep(Duration::from_secs(3));
        let screen = session.screen();
        let raw = session.transcript();
        if evidence {
            let _ = std::fs::write(evidence_dir.join(format!("{}.txt", shape.name)), &screen);
            let _ = std::fs::write(evidence_dir.join(format!("{}.raw", shape.name)), &raw);
        }

        // The frame exists and carries the theme's structure.
        check(
            &format!("{} renders a themed frame", shape.name),
            screen.contains("agents") && screen.contains("alpha"),
            "sidebar missing from the frame",
        );

        // What the terminal actually received.
        let truecolor_present = contains_subslice(&raw, b"\x1b[38;2;");
        if shape.expect_truecolor {
            check(
                &format!("{} emits truecolor as designed", shape.name),
                truecolor_present,
                "24-bit SGR missing on a truecolor terminal",
            );
        } else {
            check(
                &format!("{} never emits 24-bit SGR", shape.name),
                !truecolor_present,
                "24-bit SGR leaked onto a terminal that cannot show it",
            );
            match shape.depth {
                Depth::Ansi256 => check(
                    &format!("{} emits 256-color SGR", shape.name),
                    contains_subslice(&raw, b"\x1b[38;5;"),
                    "no 38;5;N sequence in the transcript",
                ),
                Depth::Ansi16 => check(
                    &format!("{} only uses palette indices below 16", shape.name),
                    palette_indices(&raw).all(|index| index < 16),
                    &format!(
                        "indices above 15: {:?}",
                        palette_indices(&raw).collect::<Vec<_>>()
                    ),
                ),
                Depth::Truecolor | Depth::NoColor => {}
            }
        }

        // The exact color payload the on-screen state must carry at this depth
        // (ratatui merges the foreground and the background reset into one
        // SGR, so compare parameters, not whole escapes).
        let expected = question_color.quantize(shape.depth);
        if expected == Color::None {
            check(
                &format!("{} emits no color codes at all", shape.name),
                !has_color_sgr(&raw),
                "a color SGR reached a NO_COLOR terminal",
            );
        } else {
            check(
                &format!("{} paints the question token exactly", shape.name),
                carries_color(&raw, expected),
                &format!("{expected:?} not found in the transcript's SGRs"),
            );
        }

        // ---- Layer 3: the reference HTML carries the same tokens ------------
        let theme = catalog
            .theme_with_depth("arreo", Variant::Dark, shape.depth)
            .expect("arreo loads");
        let html = reference_html(&theme);
        if evidence {
            let _ = std::fs::write(
                evidence_dir.join(format!("reference-{}.html", shape.name)),
                &html,
            );
        }
        let label = label_for(&theme, "working");
        check(
            &format!("{} reference HTML shares the TUI's tokens", shape.name),
            html.contains(&format!("data-token=\"working\" data-color=\"{label}\"")),
            &format!("HTML lacks working={label}"),
        );
    }

    // ---- The picker: `/theme` over a real terminal --------------------------
    let Some(mut session) = TuiSession::start_with(
        &tui_bin,
        &socket,
        &[],
        &[("COLORTERM", "truecolor"), ("TERM", "xterm-256color")],
    ) else {
        println!("[FAIL] theme: could not start the TUI for the picker");
        return ExitCode::FAILURE;
    };
    std::thread::sleep(Duration::from_secs(3));
    session.send("/theme");
    std::thread::sleep(Duration::from_millis(400));
    session.send("\r");
    std::thread::sleep(Duration::from_secs(1));
    let picker = session.screen();
    if evidence {
        let _ = std::fs::write(evidence_dir.join("picker-open.txt"), &picker);
    }
    let listed = ["arreo", "tokyonight", "catppuccin", "gruvbox", "system"]
        .iter()
        .filter(|name| picker.contains(**name))
        .count();
    check(
        "`/theme` opens the picker with every built-in",
        picker.contains("theme:") && listed == 5,
        &format!("{listed}/5 built-ins listed"),
    );

    // Pick the next built-in and confirm the terminal actually repainted
    // with the other palette.
    let before = session.transcript();
    session.send("j");
    std::thread::sleep(Duration::from_millis(300));
    session.send("\r");
    std::thread::sleep(Duration::from_secs(3));
    let after = session.transcript();
    let applied = session.screen();
    if evidence {
        let _ = std::fs::write(evidence_dir.join("picker-applied.txt"), &applied);
    }
    check(
        "applying a theme repaints with new colors",
        after.len() > before.len() && has_color_sgr(&after[before.len()..]),
        "no color repaint after applying a picked theme",
    );
    let names = Catalog::builtin_names();
    let picked = names.get(1).expect("at least two built-ins").clone();
    // The only pane is in `question`, so the question token is what the next
    // repaint must carry.
    let picked_question = catalog
        .theme_with_depth(&picked, Variant::Dark, Depth::Truecolor)
        .expect("picked theme loads")
        .color("question");
    check(
        &format!("the picked theme ({picked}) repaints the sidebar"),
        carries_color(&after[before.len()..], picked_question),
        &format!("{picked_question:?} never appeared in the repaint"),
    );

    session.send("q");
    std::thread::sleep(Duration::from_secs(1));
    check(
        "quit is still clean after a theme switch",
        session.exited(),
        "TUI stayed up",
    );

    drop(session);
    drop(server);
    let _ = std::fs::remove_file(&socket);
    let mut db = socket.into_os_string();
    db.push(".db");
    let _ = std::fs::remove_file(&db);

    if failures == 0 {
        println!("theme: {passes} passed, 0 failed");
        ExitCode::SUCCESS
    } else {
        println!("theme: {passes} passed, {failures} failed");
        ExitCode::FAILURE
    }
}

/// The reference HTML's label for a token at this theme's depth.
fn label_for(theme: &Theme, token: &str) -> String {
    match theme.color(token) {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Ansi(index) => format!("ansi {index}"),
        Color::None => "none".to_string(),
    }
}

/// Every `38;5;N` index in the stream (the palette the terminal was asked
/// for). Used to prove a 16-color terminal never sees an out-of-palette color.
fn palette_indices(raw: &[u8]) -> impl Iterator<Item = u16> + '_ {
    let text = String::from_utf8_lossy(raw).to_string();
    let mut out = Vec::new();
    let mut rest = text.as_str();
    while let Some(at) = rest.find("\u{1b}[38;5;") {
        rest = &rest[at + "\u{1b}[38;5;".len()..];
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(index) = digits.parse::<u16>() {
            out.push(index);
        }
    }
    out.into_iter()
}

/// The numeric parameters of every SGR sequence, in order, across the whole
/// stream. This is what a terminal actually parses.
fn sgr_params(raw: &[u8]) -> Vec<u16> {
    let text = String::from_utf8_lossy(raw);
    let mut out = Vec::new();
    let mut rest = text.as_ref();
    while let Some(at) = rest.find('\u{1b}') {
        rest = &rest[at + 1..];
        let Some(body) = rest.strip_prefix('[') else {
            continue;
        };
        let Some(end) = body.find(|c: char| c.is_ascii_alphabetic()) else {
            continue;
        };
        let (params, final_byte) = body.split_at(end);
        rest = &body[end + 1..];
        if !final_byte.starts_with('m') {
            continue;
        }
        out.extend(params.split(';').filter_map(|p| p.parse::<u16>().ok()));
    }
    out
}

/// Does the stream carry this exact color as a foreground payload?
fn carries_color(raw: &[u8], color: Color) -> bool {
    let want: Vec<u16> = match color {
        Color::Rgb(r, g, b) => vec![38, 2, u16::from(r), u16::from(g), u16::from(b)],
        Color::Ansi(index) => vec![38, 5, u16::from(index)],
        Color::None => return false,
    };
    sgr_params(raw)
        .windows(want.len())
        .any(|window| window == want)
}

/// True when the stream carries any SGR color code (fg/bg, indexed or
/// truecolor) — i.e. something a `NO_COLOR` terminal must never see.
fn has_color_sgr(raw: &[u8]) -> bool {
    let text = String::from_utf8_lossy(raw);
    let mut rest = text.as_ref();
    while let Some(start) = rest.find('\u{1b}') {
        rest = &rest[start + 1..];
        let Some(body) = rest.strip_prefix('[') else {
            continue;
        };
        let Some(end) = body.find(|c: char| c.is_ascii_alphabetic()) else {
            continue;
        };
        let (params, final_byte) = body.split_at(end);
        rest = &body[end + 1..];
        if !final_byte.starts_with('m') {
            continue;
        }
        for param in params.split(';') {
            if let Ok(code) = param.parse::<u16>() {
                // 30-37/90-97 fg, 40-47/100-107 bg, 38/48 extended.
                if (30..=38).contains(&code)
                    || (40..=48).contains(&code)
                    || (90..=107).contains(&code)
                {
                    return true;
                }
            }
        }
    }
    false
}

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}
