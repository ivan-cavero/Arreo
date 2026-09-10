# .loop/PROGRESS.md
## State snapshot          ← REWRITTEN (not appended) at every checkpoint
Task: T-0010 · cross-OS gates — DONE + PUSHED (58cd5e1; gate exit 0 + negative FAIL exit 1, evidence in .loop/evidence/T-0010/)
Where you are: T-0001/T-0002/T-0003/T-0004/T-0006/T-0007/T-0010/T-0011/T-0022 done and pushed; remote == local
Next step: next turn picks T-0005 (attach CLI — the daemon begins) or T-0008 (perf budget)
Open workers: (none)
Known broken: (none) · Parked: (none)
Findings: workspace-wide --no-default-features defeated by dependent default edges (feature unification) — lite pass is -p arreo-core; full SDKs deferred with reason in docs/cross-os.md
## Event log               ← append-only; newest last; never rewrite
- 2026-09-10 [turn 1] ledger created; repo at e489fac (docs only); T-0001 + T-0022 (AGENTS.md gardened) done
- 2026-09-10 [turn 2] T-0002 PTY manager done+pushed (342606c; 9 tests); PROMPT.md v2 synced + ADR 0001 (ef6c595)
- 2026-09-10 [turn 2] T-0003 VT state done+pushed (80fb3ab; 9 tests); ADR 0002; remote == local
- 2026-09-10 [turn 3] T-0011 fixture recorder done+pushed (55409d6; 5 fixtures, secret gate); fixed unreachable --allow-secrets arm found by adversarial pass; remote == local
- 2026-09-10 [turn 4] T-0004 state engine done+pushed (144ab67; 13 tests, question timeline Working(0)->Question(2510)); ADR 0003; remote == local
- 2026-09-10 [turn 5] T-0006 metrics done+pushed (58aae23; 7 tests, /proc+cgroup+SQLite+CLI); ADR 0004; remote == local
- 2026-09-10 [turn 6] T-0007 conpty-smoke done+pushed (8bc0d3f; real verb + 3 tests + CI step + docs); Windows proof pends on CI; remote == local
- 2026-09-10 [turn 7] T-0010 check-targets done+pushed (58cd5e1; layered gate + sqlite feature + docs + CI job); remote == local
