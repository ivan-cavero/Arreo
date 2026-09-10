# ADR 0007: socket API v1 — MessagePack cutover, one framing

- Status: accepted (2026-09-10, T-0014)
- Context: T-0005 shipped JSONL as honestly-temporary (ADR 0005), T-0013
  delivered the MessagePack codec (ADR 0006). Pillar P5 needs the full verb
  set (read/send/wait/spawn/split/attach/metrics) on one framing.
- Decision: daemon speaks framed `Message` only (Hello→Welcome handshake,
  5 s handshake timeout, per-pane Engine+Sampler wired in). `wait --state`
  is server-side (50 ms polls, exact-once answer, loud timeout). `split`
  re-spawns the recorded `SpawnSpec`. `read` is one-shot Snapshot/Delta.
  JSONL deleted everywhere (types, daemon, CLI, all tests/harnesses) — one
  framing, not two. Compat window (§7) starts at v1: JSONL was pre-v1.
- Why this one:
  - Cutover in one task (not dual-stack): two framings doubles every test,
    every client, every fuzz target — the cutover is mechanical (1:1 verbs)
    and the old tests HUNG (not errored) against frames, proving dual-stack
    would strand old clients silently. The 5 s handshake timeout turns that
    migration hazard into a loud close.
  - Server-side `wait` (not client polling): the orchestration primitive
    must not wake the client 10×/s for 5 minutes; 50 ms server polls cost
    nothing when idle and answer within the 200 ms budget on match.
  - `Pane::spawn_spec` (not client-supplied program): split means "same
    program" — the daemon is the source of truth, not the client's memory.
- Alternatives rejected:
  - Dual-stack JSONL+msgpack: doubles surface, strands old clients in hangs
    (observed, not theorized) — rejected after seeing the hang.
  - Client-side wait polling: battery/network waste, exactly what P4 exists
    to eliminate — rejected as anti-goal.
  - `split` with client-supplied program: trusts the client for server state
    — rejected (server owns panes).
- Consequences: `Pane` carries its `SpawnSpec`; daemon holds `PaneEntry`
  (pane + engine + fed cursor + sampler); F4 attach-owns-connection stands
  (fresh connection per verb — the CLI already does this); agent-skill.md
  documents the verbs with executed examples.
