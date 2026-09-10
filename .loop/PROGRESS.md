# .loop/PROGRESS.md
## State snapshot          ← REWRITTEN (not appended) at every checkpoint
Task: T-0009 · red-team chaos — DONE + PUSHED (4a74bf4; chaos 7/7 PASS, 2 product bugs fixed, evidence in .loop/evidence/T-0009/)
Where you are: 12 of 22 tasks done and pushed (Phase 0 spike complete except exit demo); remote == local
Next step: next turn picks T-0021 (phase-0 exit demo — the gate pack)
Open workers: (none)
Known broken: (none) · Parked: (none)
Findings: F1 engine UTF-8 panic fixed; F3 fork-on-worker hang fixed (spawn_blocking); F4 attach-owns-connection → T-0013/T-0014 multiplexing
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
