# .loop/PROGRESS.md
## State snapshot          ← REWRITTEN (not appended) at every checkpoint
Task: T-0023 · Noise-QUIC remote transport (phase 2, priority 2)
Why: Phase 2's identity half is done (T-0025 devices/certs/roles, T-0024 pairing that pins them), but a pinned device still has nothing to connect *to*: the daemon speaks only over the local unix socket, so "remote & security" (§4, the roadmap's differentiator) has no remote path. T-0023 is the transport that consumes `DeviceAuthority::authorize`/`check_verb` — the decision functions T-0025 shipped and tested for exactly this caller.
Approach: per the task file — quinn (pure Rust, tokio-native, no OpenSSL/BoringSSL: protects the mobile core and the §5 size budget) carrying one bidi stream per session with the existing msgpack frames (ADR 0006), inside a Noise-KK handshake (snow) whose static key is the pinned device key; the local unix socket stays the same-machine default (a second path, never a rewrite). Loopback listener behind `ARREO_TRANSPORT_TEST_LISTEN=1` is the test seam. Slice wiring + tamper/replay negatives belong to T-0027.
Deps: new crates `quinn` + `snow` and their trees. **Run all three supply-chain gates in the same commit that adds them** (`cargo vet --locked`, `cargo deny check`, `cargo audit`) — this cost two repairs already (T-0015's ratatui/paste, and the TUI deps never being exempted).
Where you are: Phase 1 closed; Phase 2 queue drafted (cdafea9; T-0023..T-0049); T-0025 done+pushed (978569a); T-0024 done+pushed (f275056); HEAD == origin/main; tree clean
Next step: read T-0023 → dep survey (quinn/snow fetch + the three gates) → frame-over-QUIC + Noise-KK tests → tamper/replay negatives → wire into the daemon behind `check_verb` → evidence → commit+push. Then T-0027 (the transport/pairing e2e slice that turns T-0023/T-0024's negatives into one command).
Open workers: (none)
Known broken: (none) · Parked: (none)
Findings:
- **The real-process test is what finds the bugs in a security flow.** T-0024's in-process unit tests were green while the actual `arreo pair` could not complete: `complete()` burned the mailbox session *before* the phone read its certificate. Two more real defects fell out of the same run (the relay expiring a session ahead of the server's own deadline; `arreo audit` never printing the event kind). Keep writing the three-process version for anything on the pairing/transport path.
- **`cargo test -p <crate>` only builds that crate's own bins.** `arreo-cli`'s pairing tests spawn the `arreo-relay` *binary* (deliberately: that keeps the AGPL crate out of the CLI's dependency graph), so the relay needed a test target of its own referencing `CARGO_BIN_EXE_arreo-relay` — otherwise a fresh CI checkout has no relay to spawn. Anything spawning another package's binary inherits this rule.
- **`xtask check-targets` earned its keep again**: unguarded `std::os::unix::net::UnixStream` in `arreo-core` broke the Windows target, and no Linux-only run would ever have noticed.
- Perf data point: `xtask bench --panes 10` came in at 267 ms this run vs ~515-615 ms earlier the same day — the box's load varies enough that only the budgets (all 6 pass comfortably) are meaningful; do not read small deltas as regressions.
- CLI-wide hazard filed as **T-0049**: every printing verb panics on a closed stdout pipe (`arreo pair | head -1` → Broken pipe, exit 101). It affects the xtask harnesses too.
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
- 2026-09-11 [turn 22] T-0024 pairing done+pushed (f275056; SPAKE2+HMAC flow in core, single-use write-once mailbox in arreo-relay served over unix+TCP, `arreo pair` both sides, `pairing_failed` audit kind, ADR 0010, 6 three-process scenarios + 28 unit tests, evidence); 4 real defects fixed en route (premature burn, expiry race, invisible audit kind, unguarded unix socket breaking the Windows gate); T-0049 filed (CLI broken-pipe panic); gates all green (211 tests, vet 279, deny 4/4, check-targets PASS/SKIP, bench 6/6); remote == local
