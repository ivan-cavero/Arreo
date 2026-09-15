//! T-0116, the server half: a theme crosses the socket as **resolved tokens**,
//! over a real daemon and a real socket.
//!
//! What only a daemon can show is the shape a client actually receives — and
//! the three answers the acceptance criteria name, none of them mocked:
//!
//! 1. **The reply is the resolution, not the document.** The theme file on this
//!    machine uses `defs` and a named reference; what arrives is a flat
//!    `token -> color` map whose every value is a literal a client with no
//!    resolver can parse (`Color::parse`), and no `defs` name is anywhere in it.
//! 2. **An unknown name is a typed refusal naming the name**, and a machine with
//!    no theme configured answers with the built-in default rather than an
//!    error.
//! 3. **One document, both depths.** The tokens are the file's own colors —
//!    unquantized — so the *same* reply renders 24-bit on a truecolor surface
//!    and inside the terminal's own 16-colour palette on a legacy one. A server
//!    that quantized at the sender would make the second surface's answer the
//!    first surface's look.
//!
//! Scratch lives under `target/test-scratch/T-0116/` (never `/tmp`: it is a
//! tmpfs here, and a SQLite log under it is a real problem).

use arreo_core::mesh::session::Client;
use arreo_core::proto::{Message, VERSION};
use arreo_core::theme::{Catalog, Color, Depth, ThemeTokens, Variant};
use arreo_server::daemon::Daemon;
use std::path::{Path, PathBuf};

/// The process-wide environment every test in this file runs against: the
/// daemon's own theme directory, an empty user config, and the update-state
/// scratch.
///
/// **One `Once`, because `set_var` is process-wide and this file's daemon
/// resolves the theme hierarchy on its own task** — a second writer racing the
/// first would be two `setenv` calls with no lock between them. One writer, then
/// reads; and because every test gets the same catalog, they cannot disagree
/// about what this machine serves.
fn isolate() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // Point the daemon's update state at a scratch directory (T-0105), so a
        // test run cannot promote and re-exec into an installed server.
        let state = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-scratch/daemon-start")
            .join(format!("state-for-tests-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&state);
        std::env::set_var("ARREO_STATE_DIR", state);

        // One theme that uses `defs`, one that varies by variant, and nothing
        // else: `pushed` is a name no built-in has, so an answer naming it can
        // only have come from this file.
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-scratch/T-0116/themes");
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(
            dir.join("pushed.json"),
            r##"{
                "$schema": "https://opencode.ai/theme.json",
                "defs": { "ink": "#123456", "paper": "#f0f0f0" },
                "theme": {
                    "primary": "ink",
                    "question": "#00ff00",
                    "text": { "dark": "paper", "light": "#101010" }
                }
            }"##,
        )
        .expect("write the pushed theme");
        std::env::set_var("ARREO_THEME_DIR", &dir);

        // No user themes in the way: the scratch directory above is the whole
        // hierarchy this process sees besides the built-ins.
        let empty = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-scratch/T-0116/no-user-config");
        let _ = std::fs::create_dir_all(&empty);
        std::env::set_var("XDG_CONFIG_HOME", &empty);
    });
}

async fn spawn_daemon(name: &str) -> (PathBuf, tokio::task::JoinHandle<()>) {
    isolate();
    let socket = std::env::temp_dir().join(format!(
        "arreo-theme-push-{name}-{}-{}.sock",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_file(&socket);
    let daemon = Daemon::new(&socket);
    let socket_for_task = socket.clone();
    let handle = tokio::spawn(async move {
        let _ = daemon.serve().await;
    });
    // The socket file appears before `serve` starts accepting; a connect that
    // races it is a connect error, not a protocol answer.
    for _ in 0..200 {
        if tokio::net::UnixStream::connect(&socket_for_task)
            .await
            .is_ok()
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    (socket, handle)
}

/// Ask the daemon for a theme and hand back its answer.
async fn ask(socket: &Path, name: &str, variant: Variant) -> Message {
    let mut client = Client::connect(socket)
        .await
        .expect("connect to the daemon");
    client
        .call(&Message::Theme {
            v: VERSION,
            name: name.to_string(),
            variant,
        })
        .await
        .expect("the daemon answers a theme request")
}

fn tokens_of(reply: &Message) -> &ThemeTokens {
    match reply {
        Message::ThemeReply { theme, .. } => theme,
        other => panic!("want a ThemeReply, got {other:?}"),
    }
}

/// **The reply is the resolved map** — every value a literal color, no `defs`
/// reference anywhere, and the base look already merged in.
#[tokio::test]
async fn a_theme_arrives_as_resolved_tokens() {
    let (socket, _daemon) = spawn_daemon("resolved").await;
    let reply = ask(&socket, "pushed", Variant::Dark).await;
    let tokens = tokens_of(&reply);
    assert_eq!(tokens.name, "pushed");
    assert_eq!(tokens.variant, Variant::Dark);

    // What the client actually receives: literals it can parse with no
    // resolver, no validator, no variant unwrapping of its own.
    for (token, value) in &tokens.tokens {
        Color::parse(value).unwrap_or_else(|e| panic!("{token}={value:?}: {e}"));
    }
    assert_eq!(
        tokens.color("primary"),
        Some("#123456"),
        "the def, resolved"
    );
    assert_eq!(tokens.color("question"), Some("#00ff00"));
    assert!(
        !tokens.tokens.values().any(|value| value == "ink"),
        "a def reference crossed the wire: {:?}",
        tokens.tokens
    );
    // A partial theme inherits the base look, and the *server* did that
    // resolution: the client gets one flat table, not a merge to perform.
    let base = Catalog::builtin()
        .tokens("arreo", Variant::Dark)
        .expect("arreo resolves");
    assert_eq!(tokens.color("done"), base.color("done"), "base inherited");
    assert_eq!(tokens.color("working"), base.color("working"));
}

/// An unknown name is refused, typed, naming the name.
#[tokio::test]
async fn an_unknown_name_is_refused_by_name() {
    let (socket, _daemon) = spawn_daemon("unknown").await;
    match ask(&socket, "nosuchtheme", Variant::Dark).await {
        Message::Error { message, .. } => {
            assert!(message.contains("nosuchtheme"), "names the name: {message}");
        }
        other => panic!("want a typed refusal, got {other:?}"),
    }
}

/// A machine with no theme configured answers with the built-in default: a
/// working answer, not a failure. The empty name is how a surface with no theme
/// of its own asks "what is this machine's theme?".
#[tokio::test]
async fn an_unconfigured_machine_answers_with_the_built_in() {
    let (socket, _daemon) = spawn_daemon("default").await;
    let reply = ask(&socket, "", Variant::Dark).await;
    let tokens = tokens_of(&reply);
    assert_eq!(tokens.name, "arreo", "the built-in default");
    assert_eq!(
        tokens.color("question"),
        Catalog::builtin()
            .tokens("arreo", Variant::Dark)
            .expect("arreo resolves")
            .color("question"),
        "and it is the built-in's own tokens"
    );
}

/// **One document, both depths.** The reply is unquantized, so a 16-colour
/// surface and a truecolor one degrade the same document differently — the
/// property T-0016 built, now over the wire.
#[tokio::test]
async fn the_same_reply_serves_both_depths() {
    let (socket, _daemon) = spawn_daemon("depths").await;
    let reply = ask(&socket, "pushed", Variant::Dark).await;
    let tokens = tokens_of(&reply);
    assert_eq!(
        tokens.color("question"),
        Some("#00ff00"),
        "the wire carries the authored color, not an index"
    );

    let truecolor = tokens.to_theme(Depth::Truecolor).expect("truecolor client");
    assert_eq!(truecolor.color("question"), Color::Rgb(0x00, 0xff, 0x00));

    let sixteen = tokens.to_theme(Depth::Ansi16).expect("16-colour client");
    let Color::Ansi(index) = sixteen.color("question") else {
        panic!(
            "a 16-colour surface must get a palette index, got {:?}",
            sixteen.color("question")
        );
    };
    assert!(index < 16, "outside the terminal's own palette: {index}");
    assert_eq!(
        truecolor.colors(),
        sixteen.colors(),
        "the same reply, two depths: depth is the surface's, not the document's"
    );
}

/// The variant the client asked for is the variant it gets, from the same file.
#[tokio::test]
async fn the_requested_variant_is_the_one_resolved() {
    let (socket, _daemon) = spawn_daemon("variant").await;
    let dark = ask(&socket, "pushed", Variant::Dark).await;
    let light = ask(&socket, "pushed", Variant::Light).await;
    let dark = tokens_of(&dark);
    let light = tokens_of(&light);
    assert_eq!(dark.variant, Variant::Dark);
    assert_eq!(light.variant, Variant::Light);
    assert_eq!(dark.color("text"), Some("#f0f0f0"), "the dark def");
    assert_eq!(light.color("text"), Some("#101010"), "the light literal");
    assert_ne!(dark.color("text"), light.color("text"));
}
