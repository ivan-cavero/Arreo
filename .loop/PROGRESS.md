## State snapshot          ← REWRITTEN (not appended) at every checkpoint
Task: T-0023 · Noise-QUIC remote transport (phase 2) — DONE, evidence recorded
Where you are: transport implemented and wired; all gates green on the final code
Next step: T-0027 — the transport/pairing e2e slice (turns T-0023/T-0024's
negatives into one command over real sockets). Then T-0026 (revocation) or
T-0029 (relay v0), whichever the queue ranks next.
Open workers: (none)
Known broken: (none) · Parked: (none)
Findings:
- **The handshake's first flight is replayable in principle — and decryption
  cannot catch it.** KK's first message depends only on the responder's *static*
  key, so a recorded flight authenticates against a fresh responder, which
  answers it and reports a session with a device that is not there. No keys are
  derivable (that needs the initiator's ephemeral secret), so nothing is readable
  or forgeable — but the server believing in a session is worth refusing. That is
  what `FlightGuard` does. Any future KK-based path needs the same guard; this is
  a property of the pattern, not of this implementation.
- **A stream framing prefix cannot be authenticated by the seal it precedes.**
  The `u16` length is outside the AEAD by construction, so a rewritten length is
  invisible to decryption. A length no seal can produce must fail fast
  (`MAX_FRAME_BYTES`); a plausible-but-longer one is indistinguishable from a
  frame still in flight and is bounded only by the transport's idle timeout.
  Worth remembering for the relay's framing (T-0029/T-0030).
- **`ring` compiles C, and that silently killed the portability gate's C-free
  fallback.** Adding rustls/ring to `arreo-core` made `check-targets` FAIL (no
  `lib.exe`) instead of SKIP, because the fallback build (`--no-default-features`
  drops `sqlite`) still pulled the crypto tree. The transport deps are now behind
  a default `transport` feature; `check-targets` is back to PASS/SKIP. Any future
  C dependency in core must keep a feature-gated pure-Rust surface, or the gate
  loses its meaning.
- **`cargo vet regenerate exemptions` is the repair path** for a new dependency
  tree (336 exemptions now, was 279); the registry imports (`isrg`,
  `bytecode-alliance`) carry no audits for this tree. `cargo audit` is clean.
- **A passing test can pass for the wrong reason.** The first tamper test ignored
  a `timeout` result and asserted only "no plaintext arrived" — also true when
  the read *times out*. It hid a real stall. Assertions on absence need a
  companion assertion that the thing actually finished.
- Perf data point: release, loopback — QUIC connect 3.0 ms, QUIC+Noise 5.1 ms
  (Noise adds one round trip), 1 MB through the transport 9.7 ms, RSS 22.7 MB
  with a listener plus five live sessions. The §5 budget (30 panes + 5 clients)
  is 120 MB; the transport's share is in the low tens of MB.
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
- 2026-09-11 [turn 23] T-0023 remote transport done+pushed: Noise-KK (snow) inside QUIC (quinn), one bidi stream carrying the T-0013 msgpack frames; Noise static derived from the pinned ed25519 identity (ADR 0011); per-verb gate (`DeviceAuthority::check_verb`) in front of the *same* `serve_session` loop the unix socket runs; zero inbound ports by default (loopback test seam only). 15 core + 5 daemon tests over real streams/sockets; 6 real defects fixed (resolver id spelling, replay guard, pump request/response deadlock, impossible frame length, quiet-peer accept starvation, swallowed failure reason); `transport` feature gate keeps `check-targets` at PASS/SKIP; vet exemptions regenerated 279->336; 236 workspace tests, clippy/fmt clean, deny/audit green, bench 6/6, all five e2e slices green; evidence in `.loop/evidence/T-0023/`; remote == local
