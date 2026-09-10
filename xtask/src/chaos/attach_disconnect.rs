//! Chaos probe 6 (attach disconnect): a vanishing client must not disturb
//! the pane or the daemon; reattach sees intact scrollback.
//!
//! Uses the real `Daemon` over a temp Unix socket with framed MessagePack
//! clients (Hello handshake each): attach, read one delta, drop the
//! connection mid-stream, reattach.

use arreo_core::proto::{codec, Message, VERSION};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

fn socket_path() -> PathBuf {
    std::env::temp_dir().join(format!("arreo-chaos-{}.sock", std::process::id()))
}

/// Framed client: connects, handshakes, and returns stream + read buffer.
fn client() -> Result<(UnixStream, Vec<u8>), String> {
    let path = socket_path();
    let mut stream = UnixStream::connect(&path).map_err(|e| format!("connect: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| format!("timeout: {e}"))?;
    let hello = Message::Hello {
        v: VERSION,
        client: "chaos".to_string(),
        wants: vec![VERSION],
    };
    stream
        .write_all(&codec::encode_frame(&hello).map_err(|e| format!("encode: {e}"))?)
        .map_err(|e| format!("write: {e}"))?;
    stream.flush().map_err(|e| format!("flush: {e}"))?;
    let mut acc = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n: usize = stream.read(&mut chunk).map_err(|e| format!("read: {e}"))?;
        if n == 0 {
            return Err("handshake closed".to_string());
        }
        acc.extend_from_slice(&chunk[..n]);
        if let Ok((Message::Welcome { .. }, consumed)) = codec::decode_frame(&acc) {
            acc.drain(..consumed);
            return Ok((stream, acc));
        }
    }
}

fn send(stream: &mut UnixStream, message: &Message) -> Result<(), String> {
    stream
        .write_all(&codec::encode_frame(message).map_err(|e| format!("encode: {e}"))?)
        .map_err(|e| format!("write: {e}"))?;
    stream.flush().map_err(|e| format!("flush: {e}"))
}

fn recv(stream: &mut UnixStream, acc: &mut Vec<u8>) -> Result<Message, String> {
    let mut chunk = [0u8; 8192];
    loop {
        if let Ok((message, consumed)) = codec::decode_frame(acc) {
            acc.drain(..consumed);
            return Ok(message);
        }
        let n: usize = stream.read(&mut chunk).map_err(|e| format!("read: {e}"))?;
        if n == 0 {
            return Err("connection closed".to_string());
        }
        acc.extend_from_slice(&chunk[..n]);
    }
}

fn spawn_msg() -> Message {
    Message::Spawn {
        v: VERSION,
        id: "chaos-disc".to_string(),
        program: "/bin/sh".to_string(),
        args: vec![
            "-c".to_string(),
            "echo MARK-1 && echo MARK-2 && sleep 30".to_string(),
        ],
        cols: 80,
        rows: 24,
    }
}

fn is_ok(message: &Message) -> bool {
    matches!(message, Message::Ok { .. })
}

fn text_of(message: &Message) -> String {
    match message {
        Message::Delta { lines, .. } | Message::Snapshot { lines, .. } => lines.join("\n"),
        Message::Exited { .. } => "exited".to_string(),
        Message::Error { message, .. } => format!("error:{message}"),
        other => format!("{other:?}"),
    }
}

pub fn run() -> Result<String, String> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("runtime: {e}"))?;
    rt.block_on(async {
        let path = socket_path();
        let _ = std::fs::remove_file(&path);
        let daemon = arreo_server::Daemon::new(&path);
        let registry = daemon.registry();
        let server = tokio::spawn(async move {
            let _ = daemon.serve().await;
        });
        tokio::time::sleep(Duration::from_millis(300)).await;

        // Spawn a pane that prints markers then idles.
        let (mut writer, mut acc) = client()?;
        send(&mut writer, &spawn_msg())?;
        let reply = recv(&mut writer, &mut acc).map_err(|e| format!("spawn-recv: {e}"))?;
        if !is_ok(&reply) {
            return Err(format!("spawn failed: {reply:?}"));
        }

        // Attach, read until MARK-2, then VANISH mid-stream (drop).
        send(
            &mut writer,
            &Message::Attach {
                v: VERSION,
                id: "chaos-disc".to_string(),
                from_line: 0,
            },
        )?;
        let mut saw = false;
        for _ in 0..50 {
            let message = recv(&mut writer, &mut acc).map_err(|e| format!("attach-recv: {e}"))?;
            let text = text_of(&message);
            if text.contains("MARK-2") {
                saw = true;
                break;
            }
            if text.contains("exited") {
                break;
            }
        }
        if !saw {
            return Err("never saw MARK-2".to_string());
        }
        drop(writer);
        tokio::time::sleep(Duration::from_millis(300)).await;

        // Reattach from 0: full scrollback must be intact.
        let (mut writer, mut acc) = client()?;
        send(
            &mut writer,
            &Message::Attach {
                v: VERSION,
                id: "chaos-disc".to_string(),
                from_line: 0,
            },
        )?;
        let mut all = String::new();
        for _ in 0..50 {
            match recv(&mut writer, &mut acc) {
                Ok(message) => {
                    let text = text_of(&message);
                    all.push_str(&text);
                    if all.contains("MARK-2") {
                        break;
                    }
                    if text.contains("exited") {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        if !(all.contains("MARK-1") && all.contains("MARK-2")) {
            return Err(format!("scrollback damaged after disconnect: {all}"));
        }

        // NOTE (v0 semantics, chaos-documented): an attached connection is
        // owned by the stream until exit — pipelining another request on it
        // hangs by design (T-0013 multiplexes). Kill on a FRESH connection.
        drop(writer);
        let (mut writer, mut acc) = client()?;
        send(
            &mut writer,
            &Message::Kill {
                v: VERSION,
                id: "chaos-disc".to_string(),
            },
        )?;
        let reply = recv(&mut writer, &mut acc).map_err(|e| format!("kill-recv: {e}"))?;
        if !is_ok(&reply) {
            return Err(format!("kill after disconnect failed: {reply:?}"));
        }
        let _ = registry;
        server.abort();
        Ok("attach disconnect: reattach intact, daemon healthy".to_string())
    })
}
