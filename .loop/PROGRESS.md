# .loop/PROGRESS.md
## State snapshot          ← REWRITTEN (not appended) at every checkpoint
Task: T-0006 · metrics sampler — DONE + PUSHED (58aae23; 7 tests green, evidence in .loop/evidence/T-0006/)
Where you are: T-0001/T-0002/T-0003/T-0004/T-0006/T-0011/T-0022 done and pushed; remote == local
Next step: next turn picks T-0005 (attach CLI, needs daemon design) or T-0007 (conpty) or T-0010 (cross-os gates)
Open workers: (none)
Known broken: (none) · Parked: (none)
Findings: polling =3.7.0 pin stands (ADR 0002); rusqlite bundled required (no system headers); daemon-backed `arreo metrics <pane>` deferred to T-0005/T-0012 (box unchecked in T-0006 file)
## Event log               ← append-only; newest last; never rewrite
- 2026-09-10 [turn 1] ledger created; repo at e489fac (docs only); T-0001 + T-0022 (AGENTS.md gardened) done
- 2026-09-10 [turn 2] T-0002 PTY manager done+pushed (342606c; 9 tests); PROMPT.md v2 synced + ADR 0001 (ef6c595)
- 2026-09-10 [turn 2] T-0003 VT state done+pushed (80fb3ab; 9 tests); ADR 0002; remote == local
- 2026-09-10 [turn 3] T-0011 fixture recorder done+pushed (55409d6; 5 fixtures, secret gate); fixed unreachable --allow-secrets arm found by adversarial pass; remote == local
- 2026-09-10 [turn 4] T-0004 state engine done+pushed (144ab67; 13 tests, question timeline Working(0)->Question(2510)); ADR 0003; remote == local
- 2026-09-10 [turn 5] T-0006 metrics done+pushed (58aae23; 7 tests, /proc+cgroup+SQLite+CLI); ADR 0004; remote == local
