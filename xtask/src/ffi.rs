//! T-0104: `cargo xtask ffi --check` — build the UniFFI surface, generate both
//! foreign languages, and assert what came out.
//!
//! One sentence: build the cdylib the bindings are read from, generate Swift and
//! Kotlin from it, assert the generated surface actually contains the client API
//! (a golden-symbol check, so "it generated an empty file" cannot pass), then
//! compile what was generated where a toolchain exists and report an honest SKIP
//! where it does not — the `check-targets` pattern.
//!
//! ## The steps, and which of them can fail
//!
//! 1. **Build** `arreo-core-ffi`'s cdylib. A real build; a failure is a FAIL.
//! 2. **Generate both languages** into scratch. A generator failure is a FAIL.
//! 3. **Assert the generated surface** against a golden symbol list — the FFI
//!    symbol names, which are identical in both languages (`uniffi_<crate>_fn_
//!    func_<name>`), so one list covers both outputs and the check cannot pass
//!    because a file merely exists.
//! 4. **Compile the Kotlin** if `kotlinc` is on PATH, with the JNA jar and the
//!    Kotlin stdlib on the classpath; a probe that cannot find them is a SKIP
//!    that names them, never a fabricated PASS.
//! 5. **Swift**: SKIP with the reason. `swiftc` needs macOS and Xcode, and
//!    `AGENTS.md` forbids faking a macOS result — the sanctioned place to
//!    *execute* macOS is the GitHub runner (the same class of gate as T-0090).
//!
//! `--enforce` turns a SKIP into a failure, exactly as `check-targets` does: CI
//! pre-merge runs without it while the toolchains are absent, and the matrix job
//! that has them is authority.
//!
//! ## What this gate is not
//!
//! It is not proof that the API *works* — that is `crates/arreo-core-ffi/tests/
//! contract.rs`, which drives pairing and a session through the exported surface
//! on this box, and it is the strong half. This gate proves the other thing: that
//! the surface is *reachable* from the two languages the product ships, with the
//! names and the types the generated API actually has.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// The crate whose surface this gate reads.
const CRATE: &str = "arreo-core-ffi";
/// The cdylib's file name **on this platform** (the bindgen reads the interface
/// metadata out of it, so it must be the real built artifact, not a stub).
///
/// Built from the target's own prefix/suffix rather than written out as
/// `libarreo_core_ffi.so`: a literal works only on Linux, so on the macOS runner —
/// the platform `docs/mobile.md` names as the authority that lifts the Swift SKIP —
/// step 1's existence check would fail after a successful build and the gate would
/// report FAIL for a path reason. A gate that cannot run where its own SKIPs are
/// lifted is a gate with a hole in it (review finding, T-0104).
fn cdylib_name() -> String {
    format!(
        "{}arreo_core_ffi{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    )
}

/// The FFI symbols the generated surface must contain.
///
/// These are UniFFI's own naming scheme (`uniffi_<crate>_fn_func_<name>` and
/// `..._fn_method_<object>_<method>`) rather than either language's spelling, for
/// one reason worth stating: the same list then applies to *both* outputs, so a
/// generator that dropped a function from one language and not the other is
/// caught. The list is the acceptance criterion's client surface, item by item —
/// pairing (both sides), identity and its fingerprint, the relay session's
/// connect/drain/ack, the machine list, the codec, and the theme tokens.
///
/// **It is checked in both directions**: a symbol in the list that the generated
/// output lacks fails, and so does a generated function that the list does not
/// name (see `assert_surface`). That second direction is what stops this list
/// going stale the day a function is added to the Rust side and nobody updates
/// the gate.
const GOLDEN_SYMBOLS: &[&str] = &[
    // --- pairing: the admitting half ---------------------------------------
    "uniffi_arreo_core_ffi_fn_func_pairing_server_begin",
    "uniffi_arreo_core_ffi_fn_method_pairingserverhandle_invite",
    "uniffi_arreo_core_ffi_fn_method_pairingserverhandle_code",
    "uniffi_arreo_core_ffi_fn_method_pairingserverhandle_receive",
    "uniffi_arreo_core_ffi_fn_method_pairingserverhandle_complete",
    "uniffi_arreo_core_ffi_fn_method_pairingserverhandle_abandon",
    // --- pairing: the joining half -----------------------------------------
    "uniffi_arreo_core_ffi_fn_func_pairing_invite_parse",
    "uniffi_arreo_core_ffi_fn_func_pairing_invite_uri",
    "uniffi_arreo_core_ffi_fn_func_pairing_code_random",
    "uniffi_arreo_core_ffi_fn_func_pairing_code_phrase",
    "uniffi_arreo_core_ffi_fn_func_pairing_phone_join",
    "uniffi_arreo_core_ffi_fn_method_pairingphonehandle_await_cert",
    // --- identity and its fingerprint --------------------------------------
    "uniffi_arreo_core_ffi_fn_func_device_key_from_seed",
    "uniffi_arreo_core_ffi_fn_method_devicekeyhandle_public_hex",
    "uniffi_arreo_core_ffi_fn_method_devicekeyhandle_fingerprint",
    "uniffi_arreo_core_ffi_fn_method_devicekeyhandle_display_id",
    "uniffi_arreo_core_ffi_fn_method_devicekeyhandle_sign",
    "uniffi_arreo_core_ffi_fn_func_root_key_from_seed",
    "uniffi_arreo_core_ffi_fn_method_rootkeyhandle_public_hex",
    "uniffi_arreo_core_ffi_fn_func_device_cert_issue",
    "uniffi_arreo_core_ffi_fn_func_device_cert_decode",
    "uniffi_arreo_core_ffi_fn_method_devicecerthandle_device_id",
    "uniffi_arreo_core_ffi_fn_method_devicecerthandle_fingerprint",
    "uniffi_arreo_core_ffi_fn_method_devicecerthandle_name",
    "uniffi_arreo_core_ffi_fn_method_devicecerthandle_role",
    "uniffi_arreo_core_ffi_fn_method_devicecerthandle_serial",
    "uniffi_arreo_core_ffi_fn_method_devicecerthandle_encode",
    "uniffi_arreo_core_ffi_fn_method_devicecerthandle_verify",
    "uniffi_arreo_core_ffi_fn_func_fingerprint_of_public_key",
    "uniffi_arreo_core_ffi_fn_func_identity_verify",
    "uniffi_arreo_core_ffi_fn_func_role_word",
    "uniffi_arreo_core_ffi_fn_func_role_parse",
    // --- the relay session: connect / drain / ack --------------------------
    "uniffi_arreo_core_ffi_fn_func_relay_session_dial",
    "uniffi_arreo_core_ffi_fn_func_relay_peer_parse",
    "uniffi_arreo_core_ffi_fn_method_relaysessionhandle_device_id",
    "uniffi_arreo_core_ffi_fn_method_relaysessionhandle_account",
    "uniffi_arreo_core_ffi_fn_method_relaysessionhandle_nonce",
    "uniffi_arreo_core_ffi_fn_method_relaysessionhandle_drain",
    "uniffi_arreo_core_ffi_fn_method_relaysessionhandle_ack",
    "uniffi_arreo_core_ffi_fn_method_relaysessionhandle_heartbeat",
    "uniffi_arreo_core_ffi_fn_method_relaysessionhandle_machines",
    "uniffi_arreo_core_ffi_fn_method_relaysessionhandle_next_peer",
    "uniffi_arreo_core_ffi_fn_method_relaysessionhandle_stream_to",
    "uniffi_arreo_core_ffi_fn_method_relaysessionhandle_closed",
    "uniffi_arreo_core_ffi_fn_method_relaypeerhandle_device_id",
    "uniffi_arreo_core_ffi_fn_method_relaypeerhandle_fingerprint",
    "uniffi_arreo_core_ffi_fn_method_relaystreamhandle_peer",
    "uniffi_arreo_core_ffi_fn_method_relaystreamhandle_read",
    "uniffi_arreo_core_ffi_fn_method_relaystreamhandle_write",
    "uniffi_arreo_core_ffi_fn_method_relaystreamhandle_close",
    // --- the machine list --------------------------------------------------
    "uniffi_arreo_core_ffi_fn_func_directory_cache_new",
    "uniffi_arreo_core_ffi_fn_func_presence_word",
    "uniffi_arreo_core_ffi_fn_method_directorycachehandle_mirror",
    "uniffi_arreo_core_ffi_fn_method_directorycachehandle_as_of_ms",
    "uniffi_arreo_core_ffi_fn_method_directorycachehandle_len",
    "uniffi_arreo_core_ffi_fn_method_directorycachehandle_is_empty",
    "uniffi_arreo_core_ffi_fn_method_directorycachehandle_rows",
    "uniffi_arreo_core_ffi_fn_method_directorycachehandle_lookup",
    "uniffi_arreo_core_ffi_fn_method_directorycachehandle_resolve_known",
    // --- the codec ---------------------------------------------------------
    "uniffi_arreo_core_ffi_fn_func_codec_protocol_version",
    "uniffi_arreo_core_ffi_fn_func_codec_min_version",
    "uniffi_arreo_core_ffi_fn_func_codec_max_frame_bytes",
    "uniffi_arreo_core_ffi_fn_func_codec_client_versions",
    "uniffi_arreo_core_ffi_fn_func_codec_negotiate",
    "uniffi_arreo_core_ffi_fn_func_codec_classify_op",
    "uniffi_arreo_core_ffi_fn_func_codec_op_name",
    "uniffi_arreo_core_ffi_fn_func_codec_encode",
    "uniffi_arreo_core_ffi_fn_func_codec_decode",
    "uniffi_arreo_core_ffi_fn_func_codec_encode_frame",
    "uniffi_arreo_core_ffi_fn_func_codec_decode_frame",
    "uniffi_arreo_core_ffi_fn_func_codec_frame_body_len",
    // --- the theme engine's tokens -----------------------------------------
    "uniffi_arreo_core_ffi_fn_func_theme_builtin",
    "uniffi_arreo_core_ffi_fn_func_theme_from_tokens",
    "uniffi_arreo_core_ffi_fn_method_themehandle_name",
    "uniffi_arreo_core_ffi_fn_method_themehandle_variant",
    "uniffi_arreo_core_ffi_fn_method_themehandle_depth",
    "uniffi_arreo_core_ffi_fn_method_themehandle_tokens",
    "uniffi_arreo_core_ffi_fn_method_themehandle_color",
    "uniffi_arreo_core_ffi_fn_method_themehandle_state_color",
    "uniffi_arreo_core_ffi_fn_method_themehandle_state_label_color",
    "uniffi_arreo_core_ffi_fn_method_themehandle_with_depth",
    "uniffi_arreo_core_ffi_fn_func_color_parse",
    "uniffi_arreo_core_ffi_fn_func_color_quantize",
    "uniffi_arreo_core_ffi_fn_func_color_fg_sequence",
    "uniffi_arreo_core_ffi_fn_func_color_contrast_ratio",
];

pub fn ffi(rest: &[String]) -> ExitCode {
    if rest.iter().any(|a| a == "--help" || a == "-h") {
        println!("usage: xtask ffi --check [--enforce]");
        println!("  builds arreo-core-ffi's cdylib, generates Swift + Kotlin, asserts");
        println!("  the generated surface, and compiles what a local toolchain can.");
        println!("  --enforce: a SKIP (no kotlinc, no Xcode) becomes a failure.");
        return ExitCode::SUCCESS;
    }
    if !rest.iter().any(|a| a == "--check") {
        eprintln!("xtask ffi: pass --check (the only mode; there is no interactive one)");
        return ExitCode::from(2);
    }
    let enforce = rest.iter().any(|a| a == "--enforce");
    let scratch = scratch_root();
    if let Err(e) = std::fs::create_dir_all(&scratch) {
        eprintln!("ffi --check: cannot create {}: {e}", scratch.display());
        return ExitCode::FAILURE;
    }

    // 1. Build the cdylib. A real build: a failure here is a FAIL, never a skip.
    if !build_cdylib() {
        eprintln!("ffi --check: FAIL (the cdylib did not build)");
        return ExitCode::FAILURE;
    }
    let cdylib = workspace_root().join("target/debug").join(cdylib_name());
    if !cdylib.exists() {
        eprintln!(
            "ffi --check: FAIL (the build reported success but {} is not there)",
            cdylib.display()
        );
        return ExitCode::FAILURE;
    }
    println!("ffi --check: build PASS ({})", cdylib.display());

    let mut failed = Vec::new();
    let mut skipped = Vec::new();
    // One per PASS, so the summary cannot drift from what happened.
    let mut passes = 1usize; // the build, reported above

    // 2 + 3. Generate both languages and assert the surface of each.
    for language in ["swift", "kotlin"] {
        let out_dir = scratch.join(language);
        // **The removal must succeed, and its failure must be a FAIL.** The surface
        // assertion concatenates every source file it finds under `out_dir`, so a
        // stale file surviving from an earlier run can contain a symbol the current
        // generator no longer emits — the vacuous green this whole check exists to
        // prevent (review finding, T-0104). `NotFound` is the ordinary first run.
        if let Err(e) = std::fs::remove_dir_all(&out_dir) {
            if e.kind() != std::io::ErrorKind::NotFound {
                println!(
                    "ffi --check: generate {language} FAIL (cannot clear {}: {e})",
                    out_dir.display()
                );
                failed.push(format!("generate-{language}"));
                continue;
            }
        }
        match generate(&cdylib, language, &out_dir) {
            Ok(()) => {
                println!("ffi --check: generate {language} PASS");
                passes += 1;
            }
            Err(detail) => {
                println!("ffi --check: generate {language} FAIL ({detail})");
                failed.push(format!("generate-{language}"));
                continue;
            }
        }
        let generated = match collect_sources(&out_dir) {
            Ok(text) => text,
            Err(detail) => {
                println!("ffi --check: surface {language} FAIL ({detail})");
                failed.push(format!("surface-{language}"));
                continue;
            }
        };
        match assert_surface(&generated) {
            Ok(()) => {
                println!(
                    "ffi --check: surface {language} PASS ({} golden symbols present)",
                    GOLDEN_SYMBOLS.len()
                );
                passes += 1;
            }
            Err(detail) => {
                println!("ffi --check: surface {language} FAIL ({detail})");
                failed.push(format!("surface-{language}"));
            }
        }
    }

    // 4. Compile the Kotlin where a Kotlin compiler exists.
    //
    // The path is `uniffi.toml`'s `package_name` (`dev.arreo.core`) plus the
    // module name, and it is asserted rather than probed with `.exists()`: a
    // package rename that left this path stale would otherwise skip the compile
    // step *silently* and still report the run as passing, which is exactly the
    // vacuous green this gate exists to prevent.
    let kotlin_out = scratch.join("kotlin");
    let kotlin_file = kotlin_out.join("dev/arreo/core/arreo_core_ffi.kt");
    if !kotlin_file.exists() {
        println!(
            "ffi --check: kotlin compile FAIL (the generator reported success but {} is not \
             there — if uniffi.toml's package_name changed, this path must change with it)",
            kotlin_file.display()
        );
        failed.push("kotlin-compile".to_string());
    } else {
        match kotlinc() {
            Kotlinc::Present => match compile_kotlin(&kotlin_out) {
                Ok(()) => {
                    println!("ffi --check: kotlin compile PASS");
                    passes += 1;
                }
                Err(ProbeFailure::Skip(reason)) => {
                    println!("ffi --check: kotlin compile SKIP ({reason})");
                    skipped.push("kotlin-compile".to_string());
                }
                Err(ProbeFailure::Fail(detail)) => {
                    println!("ffi --check: kotlin compile FAIL ({detail})");
                    failed.push("kotlin-compile".to_string());
                }
            },
            Kotlinc::Missing => {
                println!(
                    "ffi --check: kotlin compile SKIP (no `kotlinc` on PATH — a JDK alone cannot \
                     compile Kotlin; install the Kotlin compiler, e.g. `sdk install kotlin`, to \
                     lift this)"
                );
                skipped.push("kotlin-compile".to_string());
            }
        }
    }

    // 5. Swift: no compiler here, and one cannot be faked.
    println!(
        "ffi --check: swift compile SKIP (needs macOS + Xcode — `swiftc` is Apple-only; the \
         GitHub macOS runner is where this is executed, the same class of gate as T-0090)"
    );
    skipped.push("swift-compile".to_string());

    if !failed.is_empty() {
        eprintln!("ffi --check: FAILED steps: {}", failed.join(", "));
        return ExitCode::FAILURE;
    }
    if enforce && !skipped.is_empty() {
        eprintln!(
            "ffi --check: --enforce given but SKIPPED steps remain: {}",
            skipped.join(", ")
        );
        return ExitCode::FAILURE;
    }
    if skipped.is_empty() {
        println!("ffi --check: all steps PASS");
    } else {
        // **Counted, not derived.** This printed `5 - skipped.len()`, a formula that
        // was wrong the moment a step was added or a language gained one: the run
        // reported "3 pass, 2 skip" after five steps had passed, so the one line an
        // operator and the ledger read under-reported what was green (review finding,
        // T-0104).
        println!(
            "ffi --check: {} pass, {} skip (a machine with the toolchain is authority for skipped)",
            passes,
            skipped.len()
        );
    }
    ExitCode::SUCCESS
}

/// Step 1: build the cdylib the bindings are read from.
fn build_cdylib() -> bool {
    let status = Command::new("cargo")
        .args(["build", "-p", CRATE])
        .current_dir(workspace_root())
        .status();
    matches!(status, Ok(status) if status.success())
}

/// Step 2: `uniffi-bindgen generate --library <cdylib> --language <l> --out-dir`.
///
/// The generator is this crate's own binary behind its `cli` feature, so it is
/// version-locked to the library it reads — the whole reason it is not a
/// `cargo install`ed tool.
fn generate(cdylib: &Path, language: &str, out_dir: &Path) -> Result<(), String> {
    let output = Command::new("cargo")
        .args([
            "run",
            "-q",
            "-p",
            CRATE,
            "--features",
            "cli",
            "--bin",
            "uniffi-bindgen",
            "--",
            "generate",
            "--library",
        ])
        .arg(cdylib)
        .args(["--language", language, "--out-dir"])
        .arg(out_dir)
        .current_dir(workspace_root())
        .output()
        .map_err(|e| format!("cannot run the generator: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    Err(first_error_line(&String::from_utf8_lossy(&output.stderr)))
}

/// Step 3: the golden-symbol assertion, in both directions.
///
/// Forward: every symbol the acceptance criterion names must be present, so a
/// generator that silently produced a smaller surface fails.
///
/// Backward: every generated *function* must be named by the list, so the list
/// cannot go stale — a function added to the Rust side without a line here is a
/// failure, not a quiet omission. Object *methods* are deliberately not held to
/// the backward direction: UniFFI emits an internal `_ffi_free`-style symbol per
/// object, and pinning those would be pinning the generator's implementation
/// rather than this crate's surface.
fn assert_surface(generated: &str) -> Result<(), String> {
    let mut missing = Vec::new();
    for symbol in GOLDEN_SYMBOLS {
        if !generated.contains(symbol) {
            missing.push(*symbol);
        }
    }
    if !missing.is_empty() {
        return Err(format!(
            "{} golden symbol(s) absent from the generated surface, first: {}",
            missing.len(),
            missing[0]
        ));
    }
    let mut unnamed = Vec::new();
    for symbol in generated_symbols(generated) {
        if symbol.contains("_fn_func_") && !GOLDEN_SYMBOLS.contains(&symbol.as_str()) {
            unnamed.push(symbol);
        }
    }
    unnamed.sort();
    unnamed.dedup();
    if !unnamed.is_empty() {
        return Err(format!(
            "{} generated function(s) are not in the golden list, first: {} — add it to \
             GOLDEN_SYMBOLS (this is the check that stops the list going stale)",
            unnamed.len(),
            unnamed[0]
        ));
    }
    Ok(())
}

/// Every `uniffi_arreo_core_ffi_fn_…` identifier in a generated file.
fn generated_symbols(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    let needle = "uniffi_arreo_core_ffi_fn_";
    while let Some(at) = rest.find(needle) {
        let tail = &rest[at..];
        let end = tail
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(tail.len());
        out.push(tail[..end].to_string());
        rest = &tail[end..];
    }
    out
}

/// Every generated source file's text, concatenated.
fn collect_sources(dir: &Path) -> Result<String, String> {
    let mut files = Vec::new();
    walk(dir, &mut files).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    if files.is_empty() {
        return Err(format!(
            "the generator wrote nothing under {}",
            dir.display()
        ));
    }
    let mut text = String::new();
    for path in files {
        let body = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        text.push_str(&body);
        text.push('\n');
    }
    Ok(text)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            walk(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "kt" || e == "swift") {
            out.push(path);
        }
    }
    Ok(())
}

enum Kotlinc {
    Present,
    Missing,
}

fn kotlinc() -> Kotlinc {
    let found = Command::new("kotlinc")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success());
    if found {
        Kotlinc::Present
    } else {
        Kotlinc::Missing
    }
}

/// Why a probe could not conclude — a missing *toolchain piece* is a SKIP that
/// names it; a compiler that ran and complained is a FAIL.
enum ProbeFailure {
    Skip(String),
    Fail(String),
}

/// Step 4: compile the generated Kotlin.
///
/// The generated file needs JNA (the FFI bridge) and the Kotlin stdlib. Both are
/// probed by path rather than assumed: a `kotlinc` that cannot see them produces
/// a wall of "unresolved reference: Pointer" that looks like a generator bug, and
/// reporting *that* as a FAIL would be the fabricated-red this gate exists to
/// avoid.
fn compile_kotlin(out_dir: &Path) -> Result<(), ProbeFailure> {
    let Some(jna) = find_jar("jna") else {
        return Err(ProbeFailure::Skip(
            "the JNA jar is not on this machine (the generated Kotlin binds through JNA; install \
             it, e.g. `mvn dependency:get -Dartifact=net.java.dev.jna:jna:5.14.0`, or point \
             $ARREO_JNA_JAR at one)"
                .to_string(),
        ));
    };
    let Some(stdlib) = find_kotlin_stdlib() else {
        return Err(ProbeFailure::Skip(
            "the Kotlin stdlib jar is not on this machine (kotlinc ships one, but this probe \
             could not find it; set $ARREO_KOTLIN_STDLIB)"
                .to_string(),
        ));
    };
    // **kotlinx-coroutines is required, and it is not part of the stdlib.** UniFFI
    // emits `kotlinx.coroutines.*` imports whenever the interface has an `async`
    // function (this surface's relay exports), and `kotlinx-coroutines-core` is a
    // separate Maven artifact — so a machine that has exactly `kotlinc`, JNA and the
    // stdlib still fails to compile the generated file. Naming it here rather than
    // discovering it at the first real run is the difference between a SKIP an
    // operator can lift and one whose instructions cannot produce a PASS (review
    // finding, T-0104).
    let Some(coroutines) = find_jar("kotlinx-coroutines-core") else {
        return Err(ProbeFailure::Skip(
            "the kotlinx-coroutines-core jar is not on this machine, and the generated Kotlin \
             imports kotlinx.coroutines (this surface has async functions) — it is a separate \
             Maven artifact, not part of the stdlib; set $ARREO_KOTLINX_COROUTINES_CORE_JAR"
                .to_string(),
        ));
    };
    let classes = out_dir.join("classes");
    let _ = std::fs::create_dir_all(&classes);
    let output = Command::new("kotlinc")
        .arg(out_dir.join("dev/arreo/core/arreo_core_ffi.kt"))
        .args(["-classpath"])
        .arg(format!(
            "{}:{}:{}",
            jna.display(),
            stdlib.display(),
            coroutines.display()
        ))
        .arg("-d")
        .arg(&classes)
        .output()
        .map_err(|e| ProbeFailure::Fail(format!("cannot run kotlinc: {e}")))?;
    if output.status.success() {
        return Ok(());
    }
    Err(ProbeFailure::Fail(first_error_line(
        &String::from_utf8_lossy(&output.stderr),
    )))
}

/// The JNA jar: `$ARREO_JNA_JAR`, else a `jna-*.jar` in the usual Maven and
/// Debian locations.
fn find_jar(name: &str) -> Option<PathBuf> {
    let env_key = format!("ARREO_{}_JAR", name.to_ascii_uppercase());
    if let Some(path) = std::env::var_os(&env_key) {
        let path = PathBuf::from(path);
        return path.exists().then_some(path);
    }
    let roots = [
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".m2/repository")),
        Some(PathBuf::from("/usr/share/java")),
    ];
    for root in roots.into_iter().flatten() {
        if let Some(found) = search_jar(&root, name, 0) {
            return Some(found);
        }
    }
    None
}

/// A bounded depth-first search for `<name>-<version>.jar`. Bounded because a
/// Maven repository is deep and a full walk of it is minutes, not milliseconds.
fn search_jar(dir: &Path, name: &str, depth: usize) -> Option<PathBuf> {
    if depth > 6 || !dir.is_dir() {
        return None;
    }
    let entries = std::fs::read_dir(dir).ok()?;
    let mut subdirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            subdirs.push(path);
        } else if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with(name) && n.ends_with(".jar") && !n.contains("sources"))
        {
            return Some(path);
        }
    }
    subdirs.sort();
    for sub in subdirs {
        if let Some(found) = search_jar(&sub, name, depth + 1) {
            return Some(found);
        }
    }
    None
}

/// The Kotlin stdlib jar: `$ARREO_KOTLIN_STDLIB`, else beside `kotlinc`.
fn find_kotlin_stdlib() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("ARREO_KOTLIN_STDLIB") {
        let path = PathBuf::from(path);
        return path.exists().then_some(path);
    }
    let kotlinc = which("kotlinc")?;
    // `<sdk>/bin/kotlinc` → `<sdk>/lib/kotlin-stdlib.jar`.
    let lib = kotlinc.parent()?.parent()?.join("lib");
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&lib)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("kotlin-stdlib") && n.ends_with(".jar"))
        })
        .collect();
    entries.sort();
    entries.into_iter().next()
}

fn which(binary: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(binary))
        .find(|candidate| candidate.is_file())
}

fn first_error_line(stderr: &str) -> String {
    stderr
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("(no output)")
        .trim()
        .to_string()
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the workspace root")
        .to_path_buf()
}

fn scratch_root() -> PathBuf {
    workspace_root().join("target/test-scratch/T-0104/ffi")
}
