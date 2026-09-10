# .loop/PHASE-DONE.md — Phase 0 exit evidence

Revision: 8907edd · Date: 2026-09-10 · Command: `cargo xtask demo phase0`

Exit criteria (ROADMAP §6): 10 agents, live states, < 100 MB RSS,
TUI-less raw CLI — proven on Linux; Windows/macOS via CI matrix
(nightly-bench + ci artifacts, must-confirm on push).

Legs (26.333270579s):
- [x] bench-10-panes — 6/6 budget checks
- [x] live-daemon-10 — 10 spawned, attach streamed, states live, all reaped
- [x] chaos — chaos: 7 passed, 0 failed
- [x] conpty-smoke — PASS (local backend)
- [x] check-targets — PASS/SKIP (CI authoritative)

Gates: `cargo test --workspace` green · `cargo clippy --workspace --all-targets -D warnings` clean ·
`cargo fmt --check` clean · `cargo xtask bench` 6/6 · `cargo xtask e2e --slice chaos` 7/7 ·
`cargo xtask conpty-smoke` PASS · `cargo xtask check-targets` PASS/SKIP.
