//! `arreo-server` daemon binary (T-0005).
//!
//! Usage: `arreo-server [--socket PATH]`. Default socket:
//! `$XDG_RUNTIME_DIR/arreo.sock`, else `/tmp/arreo-<uid>.sock`.

use std::path::PathBuf;

#[tokio::main]
async fn main() {
    let mut socket: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1).peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--socket" => {
                socket = args.next().map(PathBuf::from);
            }
            "--help" | "-h" => {
                println!("usage: arreo-server [--socket PATH]");
                return;
            }
            other => {
                eprintln!("arreo-server: unknown flag {other}");
                std::process::exit(2);
            }
        }
    }
    let socket = socket.unwrap_or_else(default_socket);
    let daemon = arreo_server::Daemon::new(&socket);
    eprintln!("arreo-server: serving on {}", socket.display());
    if let Err(e) = daemon.serve().await {
        eprintln!("arreo-server: {e}");
        std::process::exit(1);
    }
}

fn default_socket() -> PathBuf {
    if let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime).join("arreo.sock");
    }
    let uid = libc_uid();
    std::env::temp_dir().join(format!("arreo-{uid}.sock"))
}

#[cfg(unix)]
fn libc_uid() -> u32 {
    unsafe {
        extern "C" {
            fn getuid() -> u32;
        }
        getuid()
    }
}

#[cfg(not(unix))]
fn libc_uid() -> u32 {
    0
}
