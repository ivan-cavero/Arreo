---
id: T-0009
title: Red-team pass on the spike — break the daemon on purpose
phase: 0
priority: 5
status: done
depends_on: [T-0005]
scope:
  - crates/arreo-core/**
  - crates/arreo-server/**
  - xtask/src/chaos/**
  - xtask/src/main.rs
  - xtask/Cargo.toml
---

## Scope note (re-scoped by loop, turn 10)

`xtask/src/main.rs` (e2e `--slice` dispatch) and `xtask/Cargo.toml`
(arreo-server + tokio deps for the in-process daemon probe) are outside the
letter but required by the criterion command itself. Reason written here.

## Goal

The adversarial pass the whole methodology demands (PROMPT.md §6): try to break what the
spike built — every finding becomes a task with repro steps, or gets fixed in-scope if small.

## Acceptance criteria

- [x] Chaos suite (`xtask e2e --slice chaos`): kill the daemon mid-write (scrollback never
      lost), spam 10k rapid resizes, OOM-shaped giant output line, binary garbage into the
      VT parser, two writers racing on `pane send`, socket disconnect during attach.
      7/7 PASS: mid-write-kill, resize-spam, giant-line, binary-garbage,
      racing-senders, attach-disconnect, fuzz-corpus. Evidence:
      `.loop/evidence/T-0009/chaos.txt`.
- [x] Every failure found: fixed in-scope or logged as a Phase 1 task with exact repro
      steps. No silent findings. Findings:
  - F1 (product panic, FIXED): state engine TEXT_CAP drain split multibyte
    chars → `is_char_boundary` assertion. Repro: feed ≥ 64 KiB of CJK through
    `Engine::feed`. Fix: floor truncation to char boundary
    (`crates/arreo-core/src/state/engine.rs`) + regression test
    `multibyte_truncation_never_panics`.
  - F2 (probe artifact, not product): mid-write-kill asserted `line-1\n` but
    PTY ONLCR yields `\r\n`. Probe fixed to match `line-1`.
  - F3 (product hang, FIXED): `Pane::spawn` (fork) deadlocked on multi-thread
    tokio workers — daemon answered List but never Spawn in-process. Repro:
    serve + spawn on `flavor = "multi_thread"`. Fix: `spawn_blocking` in the
    daemon Spawn arm (`crates/arreo-server/src/daemon.rs`) + regression test
    `spawn_answers_on_multithread_runtime`. Note: the standalone binary
    (multi-thread `#[tokio::main]`) was unaffected in manual tests — the hang
    needed spawn+serve sharing one runtime.
  - F4 (v0 protocol semantics, DOCUMENTED for T-0013/T-0014): an attached
    connection is owned by the stream — pipelining another request on it
    hangs (server never reads while streaming). Repro: attach then send kill
    on the same connection. v0 rule: one connection per stream; open a fresh
    connection for control ops. T-0013/T-0014 own multiplexing.
- [x] Fuzz the VT parser + state engine inputs (cargo-fuzz or a deterministic corpus loop).
      Deterministic seeded corpus (xorshift, seed `0x5eedc0de`, 4000 iters,
      131671 bytes, alt-screen toggles + resync lines): no panic, bounded,
      pageable. Chosen over cargo-fuzz (no new dep, reproducible forever).

## Verification

```console
cargo xtask e2e --slice chaos
```

## Notes

The phase is not "prove the daemon" until somebody tried hard to make it fall over.
