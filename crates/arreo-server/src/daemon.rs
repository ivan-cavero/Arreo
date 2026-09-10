//! Daemon: owns `Arc<Pane>` per id, serves the v0 protocol concurrently.
//!
//! One sentence: the daemon is a pane registry behind a UnixListener; every
//! connection is an independent tokio task, and attach streams deltas until
//! the child exits or the client goes away.
//!
//! Concurrency: `Arc<RwLock<HashMap<id, Arc<Pane>>>>`. Pane is Sync (T-0002),
//! so readers never block writers. Slow clients: bounded 100 ms poll loop per
//! attach — a stuck client delays only its own task, never the daemon.

use arreo_core::pty::{ExitState, Pane};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::RwLock;

use super::protocol::{PaneInfo, Request, Response, VERSION};

#[derive(Debug, Error)]
pub enum DaemonError {
    #[error("daemon io: {0}")]
    Io(#[from] std::io::Error),
    #[error("pane {0:?} already exists")]
    Exists(String),
    #[error("pane {0:?} not found")]
    NotFound(String),
    #[error("pty: {0}")]
    Pty(String),
}

/// Shared pane registry.
pub type Registry = Arc<RwLock<HashMap<String, Arc<Pane>>>>;

pub struct Daemon {
    registry: Registry,
    socket: PathBuf,
}

impl Daemon {
    #[must_use]
    pub fn new(socket: &Path) -> Self {
        Self {
            registry: Arc::new(RwLock::new(HashMap::new())),
            socket: socket.to_path_buf(),
        }
    }

    #[must_use]
    pub fn registry(&self) -> Registry {
        Arc::clone(&self.registry)
    }

    /// Serve forever (until the listener errors fatally). Removes a stale
    /// socket file first (previous crash) — safe: bind would fail otherwise,
    /// and a live daemon holds the path (we check by connecting first).
    pub async fn serve(&self) -> Result<(), DaemonError> {
        if Self::is_live(&self.socket).await {
            return Err(DaemonError::Io(std::io::Error::new(
                std::io::ErrorKind::AddrInUse,
                format!("socket {} already served", self.socket.display()),
            )));
        }
        let _ = std::fs::remove_file(&self.socket);
        let listener = UnixListener::bind(&self.socket)?;
        loop {
            let (stream, _) = listener.accept().await?;
            let registry = Arc::clone(&self.registry);
            tokio::spawn(async move {
                if let Err(e) = handle(stream, registry).await {
                    eprintln!("daemon: connection error: {e}");
                }
            });
        }
    }

    /// Probe: is a daemon answering at this socket?
    async fn is_live(socket: &Path) -> bool {
        if UnixStream::connect(socket).await.is_err() {
            return false;
        }
        true
    }
}

async fn write_line(
    writer: &mut (impl AsyncWriteExt + Unpin),
    value: &Response,
) -> std::io::Result<()> {
    let mut line = serde_json::to_string(value).unwrap_or_else(|_| {
        serde_json::to_string(&Response::Error {
            v: VERSION,
            message: "encode error".to_string(),
        })
        .unwrap_or_else(|_| r#"{"op":"error","v":0,"message":"encode error"}"#.to_string())
    });
    line.push('\n');
    writer.write_all(line.as_bytes()).await?;
    writer.flush().await
}

fn check_version(v: u32) -> Result<(), Response> {
    if v == VERSION {
        Ok(())
    } else {
        Err(Response::Error {
            v: VERSION,
            message: format!("unsupported version {v} (server speaks {VERSION})"),
        })
    }
}

async fn handle(stream: UnixStream, registry: Registry) -> Result<(), DaemonError> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let request: Request = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(e) => {
                write_line(
                    &mut writer,
                    &Response::Error {
                        v: VERSION,
                        message: format!("bad request: {e}"),
                    },
                )
                .await?;
                continue;
            }
        };
        // Attach streams until exit/close: handle inline, then continue
        // serving further requests on the same connection afterwards.
        if let Request::Attach { v, id, from_line } = &request {
            if let Err(response) = check_version(*v) {
                write_line(&mut writer, &response).await?;
                continue;
            }
            stream_attach(&mut writer, &registry, id, *from_line).await?;
            continue;
        }
        let response = dispatch(&request, &registry).await;
        write_line(&mut writer, &response).await?;
    }
    Ok(())
}

async fn dispatch(request: &Request, registry: &Registry) -> Response {
    match request {
        Request::Spawn {
            v,
            id,
            program,
            args,
            cols,
            rows,
        } => {
            if let Err(response) = check_version(*v) {
                return response;
            }
            // Fork off the async worker: posix_openpt+fork inside a
            // multi-threaded tokio worker can hang (chaos-found, T-0009).
            // spawn_blocking runs it on a dedicated thread instead.
            let program = program.clone();
            let args_owned = args.clone();
            let (cols, rows) = (*cols, *rows);
            let spawned = tokio::task::spawn_blocking(move || {
                let args_ref: Vec<&str> = args_owned.iter().map(String::as_str).collect();
                Pane::spawn(&program, &args_ref, cols, rows)
            })
            .await;
            let pane = match spawned {
                Ok(Ok(pane)) => Arc::new(pane),
                Ok(Err(e)) => {
                    return Response::Error {
                        v: VERSION,
                        message: format!("spawn failed: {e}"),
                    };
                }
                Err(e) => {
                    return Response::Error {
                        v: VERSION,
                        message: format!("spawn task failed: {e}"),
                    };
                }
            };
            let mut registry = registry.write().await;
            if registry.contains_key(id) {
                return Response::Error {
                    v: VERSION,
                    message: format!("pane {id:?} already exists"),
                };
            }
            registry.insert(id.clone(), pane);
            Response::Ok { v: VERSION }
        }
        Request::List { v } => {
            if let Err(response) = check_version(*v) {
                return response;
            }
            let registry = registry.read().await;
            let mut panes: Vec<PaneInfo> = registry
                .iter()
                .map(|(id, pane)| PaneInfo {
                    id: id.clone(),
                    alive: matches!(pane.try_wait(), ExitState::Running),
                })
                .collect();
            panes.sort_by(|a, b| a.id.cmp(&b.id));
            Response::Panes { v: VERSION, panes }
        }
        Request::Send { v, id, data } => {
            if let Err(response) = check_version(*v) {
                return response;
            }
            let registry = registry.read().await;
            match registry.get(id) {
                Some(pane) => match pane.send(data.as_bytes()) {
                    Ok(()) => Response::Ok { v: VERSION },
                    Err(e) => Response::Error {
                        v: VERSION,
                        message: format!("send failed: {e}"),
                    },
                },
                None => Response::Error {
                    v: VERSION,
                    message: format!("pane {id:?} not found"),
                },
            }
        }
        Request::Resize { v, id, cols, rows } => {
            if let Err(response) = check_version(*v) {
                return response;
            }
            let registry = registry.read().await;
            match registry.get(id) {
                Some(pane) => match pane.resize(*cols, *rows) {
                    Ok(()) => Response::Ok { v: VERSION },
                    Err(e) => Response::Error {
                        v: VERSION,
                        message: format!("resize failed: {e}"),
                    },
                },
                None => Response::Error {
                    v: VERSION,
                    message: format!("pane {id:?} not found"),
                },
            }
        }
        Request::Kill { v, id } => {
            if let Err(response) = check_version(*v) {
                return response;
            }
            let mut registry = registry.write().await;
            match registry.remove(id) {
                Some(pane) => {
                    drop(registry);
                    // Actually terminate the child (not just forget the Pane —
                    // otherwise it keeps running orphaned). Ignore AlreadyDead:
                    // the pane is gone from the registry either way. Reap in
                    // the background so the victim never lingers as a zombie.
                    let _ = pane.kill_shared();
                    tokio::task::spawn_blocking(move || {
                        let _ = pane.wait_timeout(std::time::Duration::from_secs(5));
                    });
                    Response::Ok { v: VERSION }
                }
                None => Response::Error {
                    v: VERSION,
                    message: format!("pane {id:?} not found"),
                },
            }
        }
        Request::Attach { .. } => Response::Error {
            v: VERSION,
            message: "attach handled inline".to_string(),
        },
    }
}

/// Stream append-deltas for `id` starting at `from_line` until the child
/// exits (then send `Exited`) or the client disconnects (write fails → return).
async fn stream_attach(
    writer: &mut (impl AsyncWriteExt + Unpin),
    registry: &Registry,
    id: &str,
    mut from_line: usize,
) -> std::io::Result<()> {
    loop {
        let pane = {
            let registry = registry.read().await;
            registry.get(id).cloned()
        };
        let Some(pane) = pane else {
            write_line(
                writer,
                &Response::Error {
                    v: VERSION,
                    message: format!("pane {id:?} not found"),
                },
            )
            .await?;
            return Ok(());
        };
        let lines = pane.drain();
        if lines.len() > from_line {
            write_line(
                writer,
                &Response::Output {
                    v: VERSION,
                    id: id.to_string(),
                    from_line,
                    lines: lines[from_line..].to_vec(),
                },
            )
            .await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "client gone"))?;
            from_line = lines.len();
        }
        match pane.try_wait() {
            ExitState::Exited(code) => {
                // Final drain already sent above (drain includes everything);
                // report the exit and close the stream.
                write_line(
                    writer,
                    &Response::Exited {
                        v: VERSION,
                        id: id.to_string(),
                        code: Some(code),
                    },
                )
                .await
                .ok();
                return Ok(());
            }
            ExitState::Running => {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}
