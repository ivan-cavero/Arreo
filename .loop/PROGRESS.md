## State snapshot          ← REWRITTEN (not appended) at every checkpoint
Task: T-0030 · durable per-device inbox (phase 2) — DONE, evidence recorded
Where you are: inbox landed and green (store v3, bounds, eviction, expiry, exactly-once with
ack+cursor, drain/ack on the wire, CLI flags, docs); 283 workspace tests
Next step: **T-0050 (daemon relay client)** — p2, and now *ready*: T-0030 was its last dependency,
so "offline is normal" has an honest answer. It is the head of the queue and the single biggest
unblocker (T-0032 remote TUI attach and T-0034's relay slice both wait on it, and T-0044's
account-join RPC lives in it too).
Open workers: (none)
Known broken: (none) · Parked: (none)
Findings:
- **A cached counter that a sweep does not update is a stale stat.** `stats()` read `queued`/`bytes`
  from `inbox_stats`, but expiry deletes rows without touching those columns, so the reported queue
  depth stayed wrong the moment anything expired (caught as "queued = 2 after everything expired").
  Queue depth is now counted from the rows; only lifetime drop counters are cached. Any
  denormalized counter needs the same scrutiny: ask which operation writes the rows but not the
  counter.
- **A test that stops reading a child's stderr kills the child.** The inbox harness's reader returned
  as soon as it parsed the bound address, dropping the pipe — the relay then died on its next
  `eprintln!` (EPIPE), which presented as an authentication failure and sent me looking at the crypto.
  Keep draining the pipe for the process's whole life. (T-0029's harness got this right by accident,
  because it accumulated the log for the opacity scan.)
- **Storing the whole framed envelope is what makes "cannot read" strongest.** The inbox keeps the
  sender's complete frame and replays it byte-for-byte on drain, so no relay-side type ever decodes
  a queued header — and the row needs no sender column, which is what keeps the schema at five
  columns and the opacity test meaningful. Rebuilding the envelope from parts would have required
  reading the header it is not supposed to read.
- **A budget row must not claim more than runs.** `cargo xtask bench` asserts six *named* checks and
  does not iterate `phase0` rows, so adding a `phase0 = true` inbox row would have been a false
  claim. The existing `queued_loss` row is annotated with the mechanism that now makes it
  observable (drop counters + the per-drain report) and stays `phase0 = false`, with the tests that
  do assert it named in the comment.
- **Both docs workers found real discrepancies worth keeping.** The relay-docs pass turned up the
  proof-payload spelling trap (fixed: the proof is over the *canonical* id, so a client may announce
  either spelling), a warning string that described the wrong trust model (the relay verifies a
  certificate chain against the account root; it does not pin devices), and a silent rate-limit
  refusal (now logged with the peer address). Delegating docs is not just prose work — it is a
  second reader on the code, and it paid for itself twice in two tasks.
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
- 2026-09-11 [turn 26] T-0030 durable per-device inbox done: store v3 (`inbox` + `inbox_stats`), bounds (10k msgs / 64 MiB / 30-day TTL, all operator-settable with validation), oldest-first eviction before the write, lazy + hourly expiry with counted drops, exactly-once stated as at-least-once + consumer `(device, seq)` dedupe with ack advancing the cursor in one transaction, `drain`/`ack` kinds and `DrainReport`/`Queued` on the wire, drained envelopes replayed byte-for-byte so the relay never decodes a stored header, CLI `--inbox-ttl-days`/`--inbox-max-messages`/`--inbox-max-mb`; 11 inbox tests (incl. real `kill -9` → restart → drain) + 14 router tests; 2 real defects fixed (stale cached queue depth after expiry; test harness killing the child via a closed stderr pipe); docs extended by a worker (protocol §4.4/4.5, deploy §8) with the at-least-once caveat stated plainly; 283 workspace tests, clippy/fmt clean, vet/deny/audit green, check-targets PASS/SKIP, five e2e slices green, bench 6/6; evidence in `.loop/evidence/T-0030/`
