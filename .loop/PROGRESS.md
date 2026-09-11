# .loop/PROGRESS.md
## State snapshot          ← REWRITTEN (not appended) at every checkpoint
Task: T-0023 · Noise-QUIC remote transport (phase 2, priority 2)
Why: T-0025 just shipped the device authority (keys, certs, roles, revocation) but nothing yet *uses* it over a network: the daemon still speaks only over the local unix socket, so "remote & security" (the roadmap's differentiator) has identity with no transport. T-0023 is the connection path that consumes `DeviceAuthority::authorize`/`check_verb` — without it Phase 2 has no remote session at all.
Approach: per the task file — quinn (pure Rust, tokio-native, no OpenSSL; mobile core + size budget) carrying one bidi stream per session with the existing msgpack frames (ADR 0006), inside a Noise-KK handshake (snow) whose static key is the pinned device key; local unix socket unchanged (a second path, never a rewrite). The loopback listener behind `ARREO_TRANSPORT_TEST_LISTEN=1` is the test seam. Slice wiring belongs to T-0027, which also carries the tamper/replay negatives.
Deps: new crates `quinn`, `snow` (and their trees) — but the vet/deny gates need a ledger note and regenerated exemptions; T-0025 already had to repair two red gates from T-0015, so verify `cargo vet --locked` + `cargo deny check` in the same commit that adds them.
Where you are: Phase 1 closed (T-0015 TUI 8bd4664, T-0016 theming 4075ad5, T-0016 resilience 7772b1b); Phase 2 queue drafted (cdafea9, T-0023..T-0048, all `proposed`); T-0025 done+pushed (978569a); HEAD == origin/main; Phase 1 exit recorded in `.loop/PHASE-DONE.md` with two human-gated items
Next step: read T-0023 → dep survey (quinn/snow fetch + vet) → frame-over-QUIC + Noise-KK tests → tamper/replay negatives → wire into the daemon behind the authority → evidence → commit+push
Open workers: (none)
Known broken: (none) · Parked: (none)
Findings:
- **T-0015 pushed two red supply-chain gates** (both repaired in 978569a): ratatui 0.29 pulled unmaintained `paste` (RUSTSEC-2024-0436) so `cargo deny check` failed, and the TUI's dependencies were never exempted so `cargo vet --locked` failed. Lesson for every dep-adding task in Phase 2: run all three supply-chain commands *before* the commit, not after.
- `cargo-vet` exemptions are the day-one floor (CONTRIBUTING rule 4), not the goal: the count went 141 → 276 with the TUI + crypto. Revisit with real `cargo vet certify` reviews when a human has the hours.
- `xtask check-targets` had been failing for x86_64-pc-windows-msvc since T-0010 (lite pass compiled `sqlite`- and Linux-only test files); fixed in 9b6f04c. The gate works now — trust its SKIP.
- Long-lived dev boxes accumulate `arreo-server` processes on `/tmp/arreo-*.sock`; reap with `pkill -f "arreo-server --socket /tmp/"` before interactive work. The committed slices clean up after themselves.
## Event log               ← append-only; newest last; never rewrite
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
- 2026-09-11 [turn 20] T-0016 theming done+pushed (4075ad5; core engine, 5 built-ins, depth fallback proven by the bytes each terminal gets, /theme picker, 27-assertion slice) + adversarial fix 7772b1b (a broken theme file no longer hides the working ones); Phase 2 queue drafted (cdafea9; T-0023..T-0048 proposed) + Phase 1 exit recorded in PHASE-DONE.md with two human-gated items; remote == local
- 2026-09-11 [turn 21] T-0025 device identity done+pushed (978569a; ed25519 certs + roles + durable authority + `arreo devices`, store v3, ADR 0009, 173 tests, evidence) — also repaired the two supply-chain gates T-0015 had left red (ratatui 0.30 drops unmaintained `paste`; vet exemptions regenerated); remote == local
