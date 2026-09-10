# ADR 0005: daemon transport — tokio Unix socket, JSONL v0, core-owned types

- Status: accepted (2026-09-10, T-0005)
- Context: first human-loop needs a client/server split now (Herdr's bet),
  but the versioned MessagePack protocol is T-0013's job. P5 (agent-native
  socket API) starts here.
- Decision: `arreo-server` = tokio `UnixListener` + `Arc<RwLock<HashMap<id,
  Arc<Pane>>>` registry; JSON-lines `{op,…}` requests with `v: 0` version
  field (rejected loudly on mismatch); attach streams append-deltas
  (`from_line` cursor) at 100 ms polls until `Exited`; request types live in
  `arreo_core::proto` (both server and CLI depend on core — the T-0001 gate
  forbids CLI→server, and the gate caught the first draft violating it).
- Why this one:
  - tokio Unix socket (not std blocking, not TCP): async per-connection
    tasks isolate slow clients; Unix permissions are the v0 auth story
    (documented, hardened in T-0012+ with Noise/pairing).
  - JSONL now (not MessagePack): zero new deps, human-debuggable
    (`python3 -c` probed the daemon directly during development), shapes
    mirror future verbs so clients port. Marked temporary in the module docs.
  - Append-deltas (not grid diff): v0 proves the split; cell-range diffing
    is T-0015's TUI work on top of T-0003's damage ranges.
- Alternatives rejected:
  - TCP+TLS now: premature (relay/Noise is Phase 2; localhost v0 needs no
    network posture beyond socket perms).
  - Embedding the daemon in the CLI process: no multi-client truth, no
    detach/reattach — defeats the task's entire point.
- Consequences: `Pane::kill_shared` (+ background reaper) exists because the
  registry holds `Arc<Pane>`; Kill actually terminates (first draft only
  forgot the pane — review caught orphaned children). Socket path:
  `$XDG_RUNTIME_DIR/arreo.sock` else `/tmp/arreo-<uid>.sock`.
