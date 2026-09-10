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

---

# .loop/PHASE-DONE.md — Phase 1 exit evidence

Revision: 4075ad5 · Date: 2026-09-11 · Status: **criteria NOT fully met** (see Open items)

Exit criteria (ROADMAP Phase 1 §6): "daily-drivable locally; OSS repo public-ready".

## Met, with evidence

- [x] **ratatui TUI (panes, sidebar, mouse)** — T-0015 (8bd4664). Pane wall + focused
      reader, sidebar grouped by state with dots/RAM, mouse-first splits (click, drag
      border), keyboard equivalent for everything. `cargo xtask e2e --slice tui`:
      17 passed, 0 failed, driving the real binary on a pty; frames in
      `.loop/evidence/T-0015/`.
- [x] **Session restore; SQLite** — T-0018 (15e42e8). v2 schema, kill -9 → restart restores
      layout + byte-equal scrollback; `cargo xtask e2e --slice persistence`.
- [x] **Theming engine (§3.12)** — T-0016 (4075ad5). opencode-shaped JSON, truecolor-first
      with 256/16/NO_COLOR fallback, 5 built-ins (`arreo`, `tokyonight`, `catppuccin`,
      `gruvbox`, `system`), `/theme` picker, shared-token reference HTML.
      `cargo xtask e2e --slice theme`: 27 passed, 0 failed across four terminal shapes
      driven on a pty (byte-exact SGR per shape); evidence in `.loop/evidence/T-0016/`.
- [x] **Universal `question` detection (§3.9)** — T-0004 + T-0017: silence + prompt-shape
      + bell, with a per-harness adapter registry (`cargo xtask adapters --check`: 15/15).
- [x] **Socket API v1 + agent skill doc** — T-0013/T-0014 (ebeb926/3696258):
      `read/send/wait/spawn/attach/metrics/split`, one framing, msgpack; `docs/agent-skill.md`;
      `cargo xtask e2e --slice api` green.
- [x] **Daemon lifecycle, metrics, budgets, enforcement, supply chain** — T-0012 (SIGTERM
      drain + crash restart), T-0006, T-0008 (6/6 budgets), T-0019 (cgroup guard + kill
      switch), T-0020 (vet/audit/deny + release profile + size probe 3.6 MB ≤ 20 MB).

## Open items (why the phase is not closed)

- [ ] **Adapters: [CC], Codex, Gemini CLI are untested.** T-0017 shipped pi + opencode with
      live-recorded fixtures; the other three run on the honest universal `default.toml`
      because no such CLI exists on this box (re-scope note in `tasks/T-0017-adapter-suite.md`).
      Needs a box with those CLIs to record fixtures — human-gated.
- [ ] **Private alpha: 20 friendly users.** Not startable by the loop; requires humans.

## Gates at this revision

`cargo test --workspace -- --skip million` → 116 passed, 0 failed ·
`cargo clippy --workspace --all-targets -- -D warnings` clean · `cargo fmt --check` clean ·
`cargo xtask bench --panes 10` 6/6 ·
`cargo xtask e2e --slice {tui,theme,api,state,lifecycle,persistence,enforcement}` green ·
`cargo xtask check-targets` linux PASS / windows SKIP (pure-Rust PASS, C deps need SDK —
CI matrix is authority) · `cargo xtask conpty-smoke` PASS (local backend).

Windows/macOS behavioral proof remains the CI matrix's job (nightly-bench + ci), per
AGENTS.md "never fake macOS results".
