# ADR 0003: state detection — explicit-clock rules engine, TOML adapters

- Status: accepted (2026-09-10, T-0004)
- Context: Pillar P4 needs working/blocked/question/idle/done per agent from
  PTY output alone, ≤ 200 ms latency, honestly labeled when inferred
  (ROADMAP §3.9 universal tier). T-0017 later adds per-harness natives.
- Decision: `Engine` in `crates/arreo-core/src/state/` — explicit `now_ms`
  clock on every method, ANSI-stripped tail matching (last 3 lines) against
  `regex`-compiled patterns from TOML adapter files (`adapters/*.toml`,
  `deny_unknown_fields` + non-empty + compilable validation).
- Why this one:
  - Explicit clock makes every timeline deterministic: tests assert exact
    `(t_ms, state)` sequences with no sleeps — CI-stable by construction,
    and the ≤ 200 ms budget holds because output events carry the feed's
    timestamp (no timer thread to lag).
  - TOML adapters (not hardcoded regexes, not WASM plugins yet): readable,
    community-extendable (T-0017's whole premise), validated loudly.
    `regex` 1.x is the standard engine, already in the tree via
    alacritty_terminal.
  - Priority question > blocked > idle on silence ticks: most actionable
    state wins; `unknown` until first evidence (never lie).
- Alternatives rejected:
  - Wall-clock/timer-thread engine: sleeps in tests, flaky CI, latency
    measured not constructed (rejected: determinism).
  - ML/heuristic classifier daemon: untestable, opaque confidence, new deps
    (rejected: simplicity gate — rules cover the universal tier).
  - Coupling to `VtPane` grid: engine must work on raw streams (adapters,
    replay, remote feeds); grid integration is a consumer choice, not a
    requirement (rejected: layering).
- Consequences: 13 tests incl. adversarial (mid-line `?`, vim screens,
  spinners, toggle recovery, terminal-done); question fixture timeline is
  exactly `Working(0) → Question(2510, inferred, "[y/n]")`.
