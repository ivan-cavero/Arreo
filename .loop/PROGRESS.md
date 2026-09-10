# .loop/PROGRESS.md
## State snapshot          ← REWRITTEN (not appended) at every checkpoint
Task: T-0016 · theming engine (phase 1, priority 5) — last Phase 1 task
Why: T-0015 landed a TUI whose colors all flow through `Theme` in `crates/arreo-tui/src/theme.rs` (const palette + `state_color`). That was the deliberate seam for T-0016: without a real theme engine the TUI cannot ship the opencode-compatible theme JSON, the depth fallback (truecolor/256/16/NO_COLOR) or the `/theme` picker, and Phase 1's "daily-drivable" exit wants a client that looks intentional on any terminal.
Approach: loader + built-ins live in `arreo-core` (`src/theme/**`, `themes/*.json` as the opencode schema), the TUI only consumes resolved tokens and swaps the selected theme at runtime via a `/theme` picker; depth detection quantizes colors with tests per depth; HTML reference render from the shared tokens so the docs and the terminal cannot drift. Evidence to `.loop/evidence/T-0016/`.
Deps: none new expected (serde/serde_json already in the workspace) — verify before adding.
Where you are: T-0015 done+pushed (8bd4664) plus a portability-gate fix (9b6f04c); HEAD == origin/main; T-0016 is the next todo
Next step: read T-0016 task file + existing Theme seam → depth/quantization tests → loader + built-ins → TUI picker → HTML reference → evidence → commit+push
Open workers: (none)
Known broken: (none) · Parked: (none)
Findings:
- `xtask check-targets` was reporting FAIL for x86_64-pc-windows-msvc at HEAD: the lite pass (`cargo check -p arreo-core --no-default-features --all-targets`) tried to compile `tests/store.rs` (needs `sqlite`) and `tests/enforce.rs` (Linux-only cgroup methods). Fixed in 9b6f04c by gating each file for what it actually needs — the intended SKIP is back.
- Long-lived dev boxes accumulate `arreo-server` processes on `/tmp/arreo-*.sock` from e2e runs; two daemons can end up "sharing" a socket path if the file is deleted before the second binds (the AddrInUse guard cannot fire on a missing path). Reap with `pkill -f "arreo-server --socket /tmp/"` before interactive TUI work; the committed slice cleans up after itself.
## Event log               ← append-only; newest last; never rewrite
- 2026-09-10 [turn 1] ledger created; repo at e489fac (docs only); T-0001 + T-0022 (AGENTS.md gardened) done
- 2026-09-10 [turn 2] T-0002 PTY manager done+pushed (342606c; 9 tests); PROMPT.md v2 synced + ADR 0001 (ef6c595)
- 2026-09-10 [turn 2] T-0003 VT state done+pushed (80fb3ab; 9 tests); ADR 0002; remote == local
- 2026-09-10 [turn 3] T-0011 fixture recorder done+pushed (55409d6; 5 fixtures, secret gate); fixed unreachable --allow-secrets arm found by adversarial pass; remote == local
- 2026-09-10 [turn 4] T-0004 state engine done+pushed (144ab67; 13 tests, question timeline Working(0)->Question(2510)); ADR 0003; remote == local
- 2026-09-10 [turn 5] T-0006 metrics done+pushed (58aae23; 7 tests, /proc+cgroup+SQLite+CLI); ADR 0004; remote == local
- 2026-09-10 [turn 6] T-0007 conpty-smoke done+pushed (8bc0d3f; real verb + 3 tests + CI step + docs); Windows proof pends on CI; remote == local
- 2026-09-10 [turn 7] T-0010 check-targets done+pushed (58cd5e1; layered gate + sqlite feature + docs + CI job); remote == local
- 2026-09-10 [turn 8] T-0005 daemon+attach done+pushed (3ea711f; JSONL socket, 5 tests, tmux evidence, 2 review bugs fixed); ADR 0005; remote == local
- 2026-09-10 [turn 9] T-0008 perf budget done+pushed (83a3c54; budget file + bench 6/6 PASS + nightly workflow); remote == local
- 2026-09-10 [turn 10] T-0009 chaos done+pushed (4a74bf4; 7 probes green, F1+F3 fixed with regression tests); remote == local
- 2026-09-10 [turn 11] T-0021 exit demo done+pushed (85324b7; demo 5/5 PASS, PHASE-DONE.md written); Phase 0 gate flagged needs-human; remote == local
- 2026-09-10 [turn 12] T-0012 lifecycle done+pushed (4c3f548; service units + SIGTERM drain + crash honesty + CI slice); remote == local
- 2026-09-10 [turn 13] T-0013 protocol done+pushed (ebeb926; msgpack codec + negotiation + 10 tests + bench probe); ADR 0006; remote == local
- 2026-09-10 [turn 14] T-0014 socket API done+pushed (3696258; msgpack cutover + 8 verbs + skill doc + api slice); ADR 0007; remote == local
- 2026-09-10 [turn 15] T-0017 adapters done+pushed (d676ab2; pi+opencode suite, 8 fixtures, registry gate, 2 bugfixes); remote == local
- 2026-09-10 [turn 16] T-0018 persistence done+pushed (15e42e8; v2 schema + restore + audit + injection fix); remote == local
- 2026-09-10 [turn 17] T-0019 enforcement done+pushed (6503ca5; cgroup guard + notify/kill + slice + CI); remote == local
- 2026-09-10 [turn 18] T-0020 supply chain done+pushed (0c48a1e; vet/audit/deny + profile + dist + size probe); remote == local
- 2026-09-11 [turn 19] T-0015 TUI done+pushed (8bd4664; arreo-tui crate, sidebar+wall+mouse-first splits, 11 unit tests, 17-assertion pty slice, 8 evidence frames, CI step) + gate fix 9b6f04c (check-targets SKIP restored); bench 6/6 PASS; remote == local; Phase-0 needs-human flag still stands
