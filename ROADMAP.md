# Arreo — Product & Technical Roadmap (v0.2)

> Name (decided): **Arreo** — the gaucho art of herding; short, CLI-friendly: `arreo status`.
> Tagline candidate: *"Run 30 agents on your server. Herd them from your phone."*

**Positioning in one sentence:** Herdr's daemon simplicity + Orca's remote/mobile ambition, with **security** and **per-agent resource truth** as first-class differentiators — in Rust, tiny on mobile, and paired with a code in under 30 seconds.

---

## 1. What we learned from the field (research summary)

### Herdr (Apache-2.0, ~37k stars)

- **Architecture:** client/server like tmux. A background server owns real PTYs; clients (TUI, CLI, raw socket) attach and render. `HeadlessServer` + `App` orchestration layer + client connections; virtual rendering pipeline; sessions restore after full server restart.
- **Agent state:** heuristics per agent CLI (different signals for `idle` / `working` / `blocked`), rolled up pane → tab → workspace. `agent start` blocks until the agent is detected and ready.
- **Agent-native API:** same surface for humans and agents — `read · send · wait · split · attach`, pane topology primitives, agent skill doc so agents learn to drive it.
- **Multi-machine:** each machine runs its own Herdr server; the client aggregates over saved SSH connections. One machine dropping does not affect others.
- **Gaps we exploit:** mobile = "coming soon" (cloud waitlist); no per-agent resource monitoring; security story = your SSH config; pricing/cloud not shipped yet.

### Orca (MIT, ~65k stars, YC/Stably)

- Desktop ADE: worktrees per task, WebGL terminal splits, embedded Chromium "design mode", GitHub/Linear native, 27 agent CLIs, usage/account tracking.
- **Mobile companion:** iOS/Android, **read-mostly** (status, recent scrollback, reply to prompt, sleep a worktree, switch accounts). One-time pairing; *desktop is the source of truth*.
- **Gaps we exploit:** the desktop app is the center of gravity (server = the app running); mobile is an accessory; no code-pairing simplicity marketing; no resource monitoring; no security story beyond trust-the-desktop.

### The wider pack (cmux, Conductor, Emdash, Superset, Claude Squad, Omnara, Vibe Kanban…)

- Conductor ($22M A) = Mac GUI wrapper around "one worktree per task".
- Most are **desktop-app-only** or **dashboard-only**; none pairs "server daemon + tiny mobile + code pairing + per-agent RAM".
- Category term still unsettled ("agent manager", "ADE", "orchestrator") — the field is young; execution speed matters more than naming.

### The open lane (our thesis)
>
> **"Herdr made agents survive you. Orca made them reachable from your pocket. Nobody made the *server* tiny, hard, and honest about resources."**

1. **Security as product**, not config: pairing-code onboarding (SPAKE2), end-to-end encryption, outbound-only server (zero open ports), auditable approvals.
2. **Resource truth:** per-agent RAM/CPU live meters, budget caps, kill switches — nobody shows this.
3. **Tiny, fast mobile app:** native SwiftUI + Jetpack Compose on a shared Rust core; not a WebView wrapper; < 25 MB.
4. **30-agent scale on modest hardware:** harness overhead must be noise compared to the agents themselves.

---

## 2. Product pillars

| # | Pillar | Commitment |
| --- | -------- | ----------- |
| P1 | **Pair in 30 seconds** | `arreo pair` shows a code/QR on the server; phone types or scans it. Done. No SSH, no ports, no config files. |
| P2 | **E2E encrypted, zero-trust relay** | Relay can route bytes but never read them. Server opens zero inbound ports. Device certs, revocation, audit log. |
| P3 | **Resource truth** | Live RAM/CPU per agent + harness; per-agent memory caps; OOM-protective auto-actions; history graphs. |
| P4 | **Semantic state, everywhere** | `working / blocked / idle / done / unknown` per agent, pushed to TUI, CLI, phone. Blocked = notification with quick-reply. |
| P5 | **Agent-native** | The socket API *is* the product surface: `read · send · wait · spawn · attach · metrics`. Agents drive Arreo like Herdr agents do. |
| P6 | **Simple core, deep when needed** | Daemon + TUI + phone = day one. Worktrees, diff review, tasks = opt-in modules (the Orca depth), never mandatory. |

---

## 3. Architecture

```text
┌──────────────────────────────────────────────────────────┐
│ machine A (your beefy server)                             │
│  [agent claude] [agent codex] [agent pi] … (×30)          │
│        ↕ PTY (portable-pty)                               │
│  ┌────────────────────────────────────────────┐           │
│  │ arreo-server daemon (Rust, single binary)  │           │
│  │  · PTY/session manager + restore           │           │
│  │  · VT state per pane (alacritty_terminal)  │           │
│  │  · state engine (heuristics + CLI hooks)   │           │
│  │  · metrics sampler (RAM/CPU per process)   │           │
│  │  · ring buffers (hot RAM) + scrollback mmap│           │
│  │  · SQLite: sessions, metrics, audit log    │           │
│  └──────────────┬─────────────────────────────┘           │
└─────────────────┼─────────────────────────────────────────┘
                  │ outbound-only, Noise-encrypted QUIC
                  ▼
        ┌──────────────────────┐
        │ arreo relay (Rust)   │  self-hostable binary or managed
        │ · rendezvous/pairing │  (SPAKE2 handshake never sees keys)
        │ · packet routing     │
        │ · push (APNs/FCM)    │
        └──────┬────────┬──────┘
               ▼        ▼
        [TUI ratatui] [CLI] [phone: SwiftUI / Compose + Rust core]
        (later: web dashboard, WASM)
```

### 3.1 Server core (the hard 70%)

- **Runtime:** tokio. One task per PTY; bounded channels everywhere; no unbounded queues (OOM discipline).
- **PTY:** `portable-pty` (WezTerm's crate; battle-tested cross-platform).
- **Terminal emulation:** `alacritty_terminal` + `vte` per pane — proven embed pattern (used by freya-terminal, teksilo, fresh-editor). Grid is compact; we never re-render what clients didn't ask for.
- **Memory discipline (30-agent budget):**
  - Hot ring buffer per pane: 512 lines in RAM (~1–3 MB), older scrollback streamed to disk, re-attached via mmap on demand.
  - Target: **server RSS ≤ 120 MB with 30 panes + 5 attached clients** (agent processes excluded — they dwarf us anyway).
  - Grid diffing: clients receive *cell-range deltas*, not frames. Idle delta traffic to a phone: < 5 KB/s.
- **State engine (3 layers, like Herdr but pluggable):**
  1. **Hooks/IPC adapters** where the CLI supports them ([CC] hooks, Codex events, Pi events — richest signal).
  2. **OSC heuristics:** OSC 133 prompt marks, OSC 9;4 progress, bell.
  3. **Output-silence + prompt regex fallback** for unknown CLIs → state `unknown`, never lie.
  - Adapter registry in TOML + community-contributable; detection latency budget ≤ 200 ms.
- **Metrics sampler:** `/proc` on Linux (cgroup v2 `memory.current` when available), sysinfo elsewhere; 1 s cadence with exponential backoff when idle; 10 s rollups to SQLite for graphs.
- **Resource enforcement:** Linux = cgroups v2 (`memory.max`, `pids.max`) per agent group, systemd-run integration; Windows = Job Objects; macOS = advisory + hard kill on exceed. A blocked agent that eats 4 GB gets throttled, and *you get told*.
- **Persistence:** sessions/topology in SQLite (WAL); restart → full layout + scrollback restore (match Herdr's killer feature).

### 3.2 Protocol & sync

- **Transport:** QUIC via `quinn`; TLS replaced by Noise handshake (`quinn-hyphae` / `reishi_quinn` pattern) so we authenticate **devices, not CAs**.
- **Framing:** MessagePack messages: `snapshot` (on attach) → `delta` streams → `resume` tokens (survive mobile network hops, subway tunnels, sleep).
- **Snapshot+delta sync** (editor-grade): phone attaches to the 30-agent overview in < 1 s on 4G because it only pulls state + last N lines, not full scrollback.
- **Backpressure:** clients declare buffer interest; server never queues more than N deltas for a slow phone — it downgrades to "digest mode" (state + last line only).

### 3.3 Pairing (the 30-second ritual)

- Server: `arreo pair` → prints `arreo: 7-gaze-iron-moon` + QR (relay URL + one-time PAKE params inside).
- Phone: scan/type → **SPAKE2** (RFC 9382) over the relay mailbox (Magic Wormhole heritage): the low-entropy code bootstraps a strong channel; an active MITM gets exactly one guess.
- Result: phone generates an ed25519 device keypair; server pins it, issues a device cert; every later connection is Noise-KK mutual auth. Code is single-use and expires in 5 min.
- Revocation: `arreo devices revoke <name>` — stolen phone? One command, dead cert, optional remote-wipe flag for server-side data.

### 3.4 Relay

- Stateless-ish Rust binary: mailbox for pairing, packet relay for NAT'd peers, push dispatcher (APNs/FCM), presence.
- **Cannot read traffic** (E2E) — this is a marketing line *and* an engineering constraint. Self-hostable single Docker image for the "I don't trust your cloud" crowd (which is also our Enterprise tier).
- Fallback path: if relay is down and both peers share a network, direct LAN mDNS discovery (bonus, not critical path).

### 3.5 Mobile app

- **Shared Rust core** (pairing, protocol, state cache, grid diffing, crypto) exposed via **UniFFI**.
- **UI (decided):** SwiftUI (iOS) + Jetpack Compose (Android), **both built in parallel from Phase 3 day one**. No WebView, no Flutter runtime. This is the "very good performance, very small app" answer — the Rust .a adds ~2–6 MB (bitdrift-style size discipline: no tokio-console, `panic=abort`, opt-level="z", strip).
- Both-platforms constraint: the 5 screens are UI-only work over the same Rust core; any protocol/crypto change lands once, in Rust. Budget +4 weeks on Phase 3 versus a single-platform start, and staff a second pair of hands (mobile collaborator) from Phase 3 kickoff.
- App budget: **< 25 MB installed**, cold start < 800 ms.
- Screens: Overview (30 agents as status cards + RAM bars) → Agent detail (state, last output, metrics graph, quick actions) → Terminal view (focused agent, virtualized grid, only when attached) → Approvals (answer prompts, approve/deny with biometric gate).
- iOS reality check: no persistent background sockets → everything rides push + snapshot-on-open; foreground live-views only the focused agent.

### 3.6 CLI/TUI

- `arreo` (attach/monitor/scripting) + TUI (ratatui): sidebar of agents w/ state + RAM, panes like Herdr, mouse-first.
- Raw socket API (Unix socket locally, QUIC remotely): `read · send · wait · spawn · split · attach · metrics` — same verbs Herdr validated, plus `metrics` which nobody has.

---

## 4. Security model (a pillar, not a feature)

**Threat model:** home/office network with hostile devices; rented VPS; stolen phone; compromised relay; curious cloud provider. *Not* (v1): compromised kernel on the server.

| Layer | Decision |
| --- | --- |
| Network posture | Server binds `127.0.0.1` only; **zero inbound ports**; all remote via outbound QUIC to relay (or LAN direct). |
| Pairing | SPAKE2 with human code; single-use, 5-min expiry; one active pairing at a time per server by default. |
| Transport | Noise-KK with pinned device certs (ed25519); handshake retry limits; no TLS-PKI to manage. |
| Authorization | Server-side ACL per device: roles `viewer / operator / admin` (v1: owner-only + viewer; roles land with Teams). |
| Audit | SQLite append-only log: every prompt sent, device, timestamp, agent touched. `arreo audit` + export. This is also a Team-tier feature. |
| Dangerous actions | Optional approval gate: match rules (sudo, `rm -rf`, `git push`) → agent blocks → phone push asks the human. |
| Secrets | OS keychain (keyring crate) on server; Secure Enclave / Android Keystore on mobile. |
| Sandboxing | Per-agent cgroup budgets (RAM, pids); optional Landlock/seccomp profile for spawned shells; git worktree isolation per task (module, later). |
| Supply chain | `cargo-dist` releases, signed (minisign/sigstore), reproducible builds; dependency audit in CI (`cargo audit`, `cargo vet`). |
| Pre-revenue gate | **Third-party security audit + public threat-model doc before charging a cent.** This is our brand: "the one that takes the server seriously". |

---

## 5. Performance budget (concrete, testable)

| Metric | Target |
| --- | --- |
| Server RSS @ 30 panes, 5 clients | ≤ 120 MB (excl. agents) |
| Per-pane harness overhead | ≤ 3 MB hot RAM |
| State detection latency | ≤ 200 ms after output |
| Phone cold attach (overview, 30 agents, 4G) | < 1 s |
| Phone idle delta traffic | < 5 KB/s |
| Focused-agent live terminal on phone | 60 fps feel via grid deltas (not raw ANSI) |
| TUI attach to 30-pane server | < 300 ms first frame |
| Crash safety | PTY scrollback never lost; server crash = restore on restart |

CI runs a nightly benchmark: 30 panes × [CC]-shaped traffic replay, asserting every budget above (this doc becomes `perf-budget.toml`).

---

## 6. Roadmap

> Solo-dev cadence assumed; compress ~40% with one collaborator. Phases overlap intentionally.

### Phase 0 — Spike (weeks 1–6) *"prove the daemon"*

- Rust skeleton: PTY manager + alacritty_terminal embed + ring buffer.
- State detection for **[CC] only** (hooks + heuristics).
- `arreo attach` CLI; RAM sampler; 10-agent load test vs budget.
- **Exit demo:** 10 agents, live states, < 100 MB RSS, TUI-less raw CLI.

### Phase 1 — Core alpha (weeks 7–16) *"self-hosted, local"*

- ratatui TUI (panes, sidebar, mouse); session restore; SQLite.
- Adapters: [CC], Codex, Pi, opencode, Gemini CLI.
- Socket API v1 (`read/send/wait/spawn/attach/metrics`) + agent skill doc.
- Private alpha: 20 friendly users (this server's owner first).
- **Exit:** daily-drivable locally; OSS repo public-ready.

### Phase 2 — Remote & security (weeks 17–26) *"the differentiator"*

- Noise/QUIC transport; SPAKE2 pairing; device certs + revocation.
- Relay v0 (self-hostable); remote TUI attach; audit log.
- cgroups limits + kill switches + metrics history.
- **Public OSS launch** (Show HN / r/rust). No pricing yet — stars and word of mouth.
- **Exit:** a stranger pairs a second machine in < 5 min without docs help.

### Phase 3 — Mobile beta (weeks 27–42) *"pocket control"*

- Rust core via UniFFI; **iOS (SwiftUI) and Android (Compose) built in parallel** — shared core means duplicated UI only.
- Pairing QR; overview; agent detail; quick answers; push notifications (blocked/done); RAM meters.
- **Exit:** the demo from the tagline — 30 agents on a server, managed from a phone (both stores' beta tracks).

### Phase 4 — ADE depth (weeks 43–54) *"Orca-competitive modules"*

- Worktree-per-task module; diff review (desktop full, mobile read + comment-back).
- Approval gates; GitHub/Linear task surfaces; plugins SDK v0.
- **Exit:** a team uses worktrees + mobile approvals as their main loop.

### Phase 5 — Monetize & GA (weeks 55–62) *"the pricing panel"*

- Managed relay (multi-region), billing (Stripe), security audit (external, published), threat model, landing + pricing page.
- Team tier (roles, org machines, SSO later).
- **Exit:** GA + first revenue.

---

## 7. Pricing (open-core; relay is the paid surface)

| Tier | Price | What's in it |
| --- | --- | --- |
| **OSS / Self-host** | Free forever | Server + TUI + CLI under **Apache-2.0**; relay under **AGPL-3.0** (decided) — unlimited agents, **self-hosted relay included**; the whole product if you run it yourself. AGPL keeps third parties from productizing the managed relay without contributing back. |
| **Cloud Personal** | **$9/mo** | Managed E2E relay (zero-config pairing, no self-hosting), push notifications, mobile sync, metrics history (30 d), device management UI. |
| **Cloud Pro** | **$19/mo** | + metrics retention (1 y), diff review on mobile, approval gates, multi-server org of one, priority relay regions, early features. |
| **Team** | **$15/user/mo** (min 5) | + shared machine registry, roles (viewer/operator/admin), audit log export, SSO (SAML/OIDC), centralized device policies. |
| **Enterprise** | Custom | On-prem relay license, SLA, security review support, priority adapter requests. |

**Unit economics:** relay + push ≈ $1–2/user/mo at small scale → ~80% gross margin. Herdr's cloud is "coming soon" — being first to a *credible, audited* paid remote story is the wedge. Free self-host keeps OSS credibility and feeds the funnel (same playbook as Tailscale).

---

## 8. Risks & mitigations

| Risk | Mitigation |
| --- | --- |
| Agent CLI churn (21+ CLIs, breaking changes) | Adapter registry as data (TOML), 3-layer detection degrades gracefully, community adapters, coverage tests in CI. |
| Herdr ships cloud first | We don't out-feature them; we out-*position* (security + resources + tiny mobile). Speed on P2/P3 is existential — protect those phases. |
| iOS background limits | Push-first design; snapshot-on-open; live view only when foregrounded. Design for it, don't fight it. |
| Solo bandwidth | Ruthless MVP cuts (no web dashboard in v1, no Windows-native polish until P2); **both-platforms mobile makes a mobile collaborator a P3 requirement, not an option** — recruit during P2. |
| Security incident / overpromising | External audit before revenue; published threat model; "can't-read-your-traffic relay" is enforceable architecture, not marketing. |
| Rust UI velocity on mobile | UIs are small: 5 screens per platform. The hard logic lives in the shared Rust core, tested off-device; any fix ships once in Rust, twice in thin UI. |

---

## 9. Decisions log

| Decision | Outcome (2026, kickoff) |
| --- | --- |
| Name | **Arreo** (decided) |
| Mobile stack | **SwiftUI + Jetpack Compose over shared Rust core via UniFFI** (decided) |
| Mobile order | **iOS and Android in parallel from Phase 3** (decided; +4 weeks, collaborator required) |
| License | **Apache-2.0 core + AGPL-3.0 relay** (decided; protects the managed service) |
| Web dashboard | **Open** — recommendation stands: defer to post-Phase 4, revisit when Team tier demand appears. |

---

*Sources: herdr.dev docs (architecture, socket API, agents, connecting machines, persistence), DeepWiki herdrdev/herdr, github.com/stablyai/orca + onorca.dev docs (mobile companion), RFC 9382 (SPAKE2), magic-wormhole docs, quinn-rs + quinn-hyphae, mozilla/uniffi-rs, bitdrift Rust-SDK binary-size post, alacritty_terminal/portable-pty embed precedents (freya-terminal, teksilo, fresh-editor), tomrochette.com orchestration feature matrix, rustman.org Conductor ecosystem survey.*
