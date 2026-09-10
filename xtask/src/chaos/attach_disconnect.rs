//! Chaos probe 6 (attach disconnect): a vanishing client must not disturb
//! the pane or the daemon; reattach sees intact scrollback.
//!
//! Uses the real `Daemon` over a temp Unix socket with raw JSONL clients:
//! attach, read one delta, drop the connection mid-stream, reattach.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

fn socket_path() -> PathBuf {
    std::env::temp_dir().join(format!("arreo-chaos-{}.sock", std::process::id()))
}

fn client() -> Result<(BufReader<UnixStream>, UnixStream), String> {
    let path = socket_path();
    let stream = UnixStream::connect(&path).map_err(|e| format!("connect: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| format!("timeout: {e}"))?;
    let reader = BufReader::new(stream.try_clone().map_err(|e| format!("clone: {e}"))?);
    Ok((reader, stream))
}

fn send(stream: &mut UnixStream, line: &str) -> Result<(), String> {
    stream
        .write_all(line.as_bytes())
        .map_err(|e| format!("write: {e}"))?;
    stream.write_all(b"\n").map_err(|e| format!("write: {e}"))?;
    stream.flush().map_err(|e| format!("flush: {e}"))
}

fn recv(reader: &mut BufReader<UnixStream>) -> Result<String, String> {
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| format!("read: {e}"))?;
    if line.is_empty() {
        return Err("connection closed".to_string());
    }
    Ok(line)
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
        let (mut reader, mut writer) = client()?;
        send(&mut writer, r#"{"op":"spawn","v":0,"id":"chaos-disc","program":"/bin/sh","args":["-c","echo MARK-1 && echo MARK-2 && sleep 30"],"cols":80,"rows":24}"#)?;
        let reply = recv(&mut reader).map_err(|e| format!("spawn-recv: {e}"))?;
        if !reply.contains("\"ok\"") {
            return Err(format!("spawn failed: {reply}"));
        }

        // Attach, read until MARK-2, then VANISH mid-stream (drop).
        send(&mut writer, r#"{"op":"attach","v":0,"id":"chaos-disc","from_line":0}"#)?;
        let mut saw = false;
        for _ in 0..50 {
            let line = recv(&mut reader).map_err(|e| format!("attach-recv: {e}"))?;
            if line.contains("MARK-2") {
                saw = true;
                break;
            }
            if line.contains("exited") {
                break;
            }
        }
        if !saw {
            return Err("never saw MARK-2".to_string());
        }
        drop(reader);
        drop(writer);
        tokio::time::sleep(Duration::from_millis(300)).await;

        // Reattach from 0: full scrollback must be intact.
        let (mut reader, mut writer) = client()?;
        send(&mut writer, r#"{"op":"attach","v":0,"id":"chaos-disc","from_line":0}"#)?;
        let mut all = String::new();
        for _ in 0..50 {
            match recv(&mut reader) {
                Ok(line) => {
                    all.push_str(&line);
                    if all.contains("MARK-2") {
                        break;
                    }
                    if line.contains("exited") {
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
        drop(reader);
        drop(writer);
        let (mut reader, mut writer) = client()?;
        send(&mut writer, r#"{"op":"kill","v":0,"id":"chaos-disc"}"#)?;
        let reply = recv(&mut reader).map_err(|e| format!("kill-recv: {e}"))?;
        if !reply.contains("\"ok\"") {
            return Err(format!("kill after disconnect failed: {reply}"));
        }
        let _ = registry;
        server.abort();
        Ok("attach disconnect: reattach intact, daemon healthy".to_string())
    })
}
