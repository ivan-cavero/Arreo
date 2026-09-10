# .loop/PROGRESS.md
## State snapshot          ← REWRITTEN (not appended) at every checkpoint
Task: T-0003 · VT state per pane — DONE + PUSHED (80fb3ab; 9 tests green, evidence in .loop/evidence/T-0003/)
Where you are: T-0001/T-0022/T-0002/T-0003 all done and pushed; remote == local
Next step: next turn picks T-0011 (fixture recorder, prio 2, unblocks T-0004) or T-0006/T-0007/T-0010
Open workers: (none)
Known broken: (none) · Parked: (none)
Findings: polling must stay =3.7.0 until alacritty upgrade (rustix 0.38 vs 1.x skew) — ADR 0002
## Event log               ← append-only; newest last; never rewrite
- 2026-09-10 [turn 1] ledger created; repo at e489fac (docs only); T-0001 + T-0022 (AGENTS.md gardened) done
- 2026-09-10 [turn 2] T-0002 PTY manager done+pushed (342606c; 9 tests); PROMPT.md v2 synced + ADR 0001 (ef6c595)
- 2026-09-10 [turn 2] T-0003 VT state done+pushed (80fb3ab; 9 tests); ADR 0002; remote == local
