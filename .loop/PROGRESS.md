# .loop/PROGRESS.md
## State snapshot          ← REWRITTEN (not appended) at every checkpoint
Task: T-0024 · SPAKE2 pairing (phase 2, priority 2) — chosen over T-0023 at equal priority
Why: T-0025 shipped the device authority (keys, certs, roles, revocation) but a user still cannot *get a device pinned* without copying a public key around by hand — the ritual §1/§3.3 promises ("pair in 30 seconds, no password, no LAN trust") does not exist, and pairing is the only production caller of `DeviceAuthority::issue`. T-0023 (the transport) also needs paired devices to be reachable at all, so pairing is the shorter path to a demonstrable Phase 2.
Approach: per the task file — `spake2` (RFC 9382 over ed25519; a PAKE is exactly where hand-written crypto dies) turns a ~32-bit 4-word code into an authenticated channel, with the relay mailbox carrying only opaque flights (Magic-Wormhole heritage), single-use session ids, an injectable TTL, and a one-guess budget burned server-side; success issues through T-0025's `DeviceAuthority::issue` so there is still exactly one cert-minting path. Rejected: code-as-PSK under plain Noise and Noise-XX keyed by the code (both leak an offline guesser).
Deps: `spake2` 0.4 (+ its tree) in `arreo-core`, per the task file's decision; the relay mailbox is std + `arreo-core` only. The vet/deny gates have to be green in the *same* commit that adds it (T-0025 had to repair two gates T-0015 left red — do not repeat that).
Where you are: Phase 1 closed (T-0015 TUI 8bd4664, T-0016 theming 4075ad5, T-0016 resilience 7772b1b); Phase 2 queue drafted (cdafea9, T-0023..T-0048, all `proposed`); T-0025 done+pushed (978569a); HEAD == origin/main; Phase 1 exit recorded in `.loop/PHASE-DONE.md` with two human-gated items
Next step: read T-0024 → add `spake2` (+ vet/deny in the same commit) → word-list code + session state machine (single-use, TTL, guess budget) → relay mailbox in `arreo-relay/src/pairing.rs` → `arreo pair` CLI → real-process test → evidence → commit+push. Then T-0023 (transport), which consumes the same authority.
Ordering note: T-0023 and T-0024 are both p2 with deps met. T-0024 first because it is self-contained (no QUIC/snow tree), it is the *only* production path that calls `DeviceAuthority::issue` besides the CLI — so it proves T-0025 end-to-end — and it leaves the heavier network dependency for a turn with more room to spare.
Open workers: (none)
Known broken: (none) · Parked: (none)
Findings:
- **T-0015 pushed two red supply-chain gates** (both repaired in 978569a): ratatui 0.29 pulled unmaintained `paste` (RUSTSEC-2024-0436) so `cargo deny check` failed, and the TUI's dependencies were never exempted so `cargo vet --locked` failed. Lesson for every dep-adding task in Phase 2: run all three supply-chain commands *before* the commit, not after.
- `cargo-vet` exemptions are the day-one floor (CONTRIBUTING rule 4), not the goal: the count went 141 → 276 with the TUI + crypto. Revisit with real `cargo vet certify` reviews when a human has the hours.
- `xtask check-targets` had been failing for x86_64-pc-windows-msvc since T-0010 (lite pass compiled `sqlite`- and Linux-only test files); fixed in 9b6f04c. The gate works now — trust its SKIP.
- Long-lived dev boxes accumulate `arreo-server` processes on `/tmp/arreo-*.sock`; reap with `pkill -f "arreo-server --socket /tmp/"` before interactive work. The committed slices clean up after themselves.
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
