## State snapshot          ← REWRITTEN (not appended) at every checkpoint
Task: T-0029 · relay v0 router (phase 2) — DONE, evidence recorded; docs worker finishing
Where you are: router landed and green (14 integration tests over the real binary, real QUIC,
real certs); ADR 0013 written; T-0029 re-scoped and marked done; T-0050 created
Next step: **T-0050 (daemon relay client)** — p2, and the recorded half of T-0029's criterion 4.
It is the head of the queue: T-0032 and T-0034 now depend on it, and it is what makes the relay
reachable by an actual machine. (T-0030 durable inbox is p2 and also ready; it is the other
unblocking half for "offline is normal".)
Open workers: RelayDocs (writing docs/relay-protocol.md + docs/relay-deploy.md; protocol spec
landed, deploy guide pending) — integrate before committing
Known broken: (none) · Parked: (none)
Findings:
- **A length prefix cannot be authenticated by the seal it precedes, and the same confusion bit
  twice more in this task.** `RelayEnvelope::encode` writes its own prefix; the relay read through
  the generic `read_frame` (which strips one) and handed `decode` a body it read as a size — the
  relay saw a 1.6 GB frame and dropped the session. The core unit test missed it by calling
  `decode` directly. Then the *same* mistake recurred inside a payload (`encode_message` prefixes,
  `decode_message` does not). Both are now named: `read_envelope` + `RelayError::Incomplete` (the
  one retryable case) for the outer frame, `encode_payload`/`decode_payload` for the inner value.
  **Lesson: when two functions differ only by a prefix, name them so the wrong pairing is
  unwritable** — a test that calls the inner function directly cannot catch the outer confusion.
- **A refusal written and immediately abandoned is not a refusal.** Returning right after
  `write_frame` dropped the connection before the peer read it, so the client reported "connection
  lost" instead of the relay's reason. The refusal path now finishes the stream and holds the
  connection briefly.
- **Device ids compared as strings again.** The wire carries `dev_<hex>`, the certificate holds
  bare hex — the same defect class T-0023 hit in the transport resolver. Parse to `DeviceId` and
  compare values. This is now the third occurrence; treat any id comparison across a wire boundary
  as suspect by default.
- **A rate limiter will catch your own tests, and that is the protection working.** Four refusals
  from one address tripped the handshake budget mid-test. The fix was to split those cases into
  independent relays and *pin* the limiter's behavior in its own test — not to weaken it.
- **A subagent can find a class of bug the parent is pattern-blind to** — worth remembering as a
  reason to keep delegating docs and reviews rather than writing everything in one head.
- **`check-targets` caught the new module immediately**: `arreo_core::relay::client` needs the QUIC
  transport, so the module (and its re-export) is now `#[cfg(feature = "transport")]`. The lesson
  from T-0023 holds: every new core module must keep a pure-Rust surface, or the gate loses meaning.
- **Re-scope recorded, not silent:** T-0029's fence gained `crates/arreo-core/src/relay/**` (the
  wire vocabulary must be Apache for T-0035's boundary), and "two real daemons" became "two real
  protocol clients" with the daemon half split out as **T-0050**. T-0032 and T-0034 gained that
  dependency. An account registry door (`arreo-relay account add`) was added because a relay with
  no registered account refuses every device — the criteria assumed it existed.
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
- 2026-09-11 [turn 24] T-0043 machine directory done+pushed: `arreo_core::mesh` (MachineId/Name rules with ASCII-only rejection, presence thresholds, canonical sorted export, read-only DirectoryCache) + `arreo-relay` SQLite (single RelayStore connection/migration owner, account/machine tables, UNIQUE(account_id,name_key), tombstones, BEGIN IMMEDIATE claims) + ADR 0012 (task file said 0009, already taken — corrected); 10 directory acceptance tests (schema denylist, explicit-join ticket, suffix conflicts, 8-thread concurrent claims, rename atomicity, tombstone hold + expiry boundary, stale prune idempotence, export round-trip) + 5 core rule tests; 1 real defect fixed (expired tombstone could never release its name); 252 workspace tests, clippy/fmt clean, vet/deny/audit green, check-targets PASS/SKIP; evidence in `.loop/evidence/T-0043/`; remote == local
- 2026-09-11 [turn 25] T-0029 relay v0 router done: Apache wire vocabulary + reference client in `arreo_core::relay` (framing, Hello/Challenge/Auth/Welcome, certificate + proof-of-possession auth, typed outcomes), AGPL router in `arreo-relay` (accept/handshake split, per-(account,device) live map, per-envelope validation, status reports, rate limiter), store v2 (account root key + relay_device registry), CLI `serve`/`account add` with the T-0024 pairing path preserved, ADR 0013; 6 core + 14 integration tests (real binary, real QUIC, real certs: opacity scan over state dir and logs, unknown account/foreign cert/no-proof/replayed-proof refusals, spoofed sender, foreign account, unknown vs offline destination, restart durability, stalled peer, reconnect token, zero-length payload, oversized frame, rate limit); 6 defects fixed (double length prefix in the envelope path, the same in a payload, lost refusal, string id comparison, no forgive-on-success, test tripped its own limiter); re-scoped with T-0050 created for the daemon half; 268 workspace tests, clippy/fmt clean, vet/deny/audit green, check-targets PASS/SKIP; evidence in `.loop/evidence/T-0029/`
