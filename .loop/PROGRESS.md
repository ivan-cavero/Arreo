# .loop/PROGRESS.md
## State snapshot          ← REWRITTEN (not appended) at every checkpoint
Task: T-0011 · fixture recorder — DONE + PUSHED (55409d6; 4 fixture + 10 pty tests green, evidence in .loop/evidence/T-0011/)
Where you are: T-0001/T-0002/T-0003/T-0011/T-0022 done and pushed; remote == local
Next step: next turn picks T-0004 (state engine — fixtures now unblock it), T-0006, T-0007, or T-0010
Open workers: (none)
Known broken: (none) · Parked: (none)
Findings: polling =3.7.0 pin stands (ADR 0002); record needs RAW bytes — drain() is lossy, hence the raw journal (re-scoped in T-0011 file)
## Event log               ← append-only; newest last; never rewrite
- 2026-09-10 [turn 1] ledger created; repo at e489fac (docs only); T-0001 + T-0022 (AGENTS.md gardened) done
- 2026-09-10 [turn 2] T-0002 PTY manager done+pushed (342606c; 9 tests); PROMPT.md v2 synced + ADR 0001 (ef6c595)
- 2026-09-10 [turn 2] T-0003 VT state done+pushed (80fb3ab; 9 tests); ADR 0002; remote == local
- 2026-09-10 [turn 3] T-0011 fixture recorder done+pushed (55409d6; 5 fixtures, secret gate); fixed unreachable --allow-secrets arm found by adversarial pass; remote == local
