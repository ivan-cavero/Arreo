# Arreo — Product & Technical Roadmap (v0.3)

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
| P7 | **Never break your flow** | Updates install without killing agents or dropping sessions; a server offline for 15 days comes back and everything resumes. Windows, Linux, macOS — all first-class from Phase 0. |

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
        │ · machine directory  │  (presence: names + online/offline only)
        │ · durable inboxes    │
        │ · push (APNs/FCM)    │
        └──────┬────────┬──────┘
               ▼        ▼
        [TUI ratatui] [CLI] [phone: SwiftUI / Compose + Rust core]
        [web dashboard (WebTransport/WebCrypto)]  (later: WASM)
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

### 3.7 Multi-machine mesh (your VPS + your Raspberry Pi, and every machine after)

> Real scenario from the owner: a VPS and a Raspberry Pi 5 each running an Arreo server; manage either from any PC, the phone, or **from the VPS into the Pi and vice versa**. Machines are first-class citizens, not SSH bookmarks.

- **Uniform protocol, three roles:** every actor speaks the same Noise-KK MessagePack protocol. A *server* is just a client with PTYs attached. So `arreo` on the VPS attaching to the Pi's agents is **the same code path** as the phone attaching to the VPS — one protocol to secure, test, and evolve.
- **Machine directory at the relay:** an account (created at first pairing, keyed to a root keypair) owns a list of machines: name, device id, online/offline presence, last-seen. The directory is metadata-only (no keys, no agent state) — routing is still E2E per machine.
- **Discovery & attach flow:** `arreo machines` lists the account's machines with presence → `arreo attach workbox` / phone tap → E2E connection (relay-routed or LAN-direct when available) → same snapshot/delta sync as local. Machine names resolve via the directory; no IPs, no SSH, no ports.
- **Cross-server trust model:** each machine independently pairs its devices (a phone paired to the VPS is *not* automatically paired to the Pi — explicit per-machine grant, or `arreo machines trust <device>` to extend). Roles (viewer/operator) evaluated on the machine that owns the agents, never at the relay.
- **Server-as-client quality bar:** remote attach from one server to another gets the same live updates, notifications, and metrics as any other client. The VPS sees the Pi's blocked agent in its own TUI sidebar and can answer it.
- **Failure isolation:** Herdr's rule adopted — one machine dropping affects nothing else; per-machine reconnect/backoff is independent.

### 3.8 Harness config sync (add a provider once, replicated everywhere)

> Real pain: adding a new provider/model means editing `opencode.jsonc`, Codex config, etc. on every PC and instance. Opt-in sync makes it an edit-once operation.

- **Watched files, opt-in per file:** `arreo.toml` declares what to sync — well-known harness configs (`opencode.jsonc`, Codex TOML, Pi settings, …) plus arbitrary paths. Sync is per-file, never a blanket folder (no surprise overwrites).
- **Mechanism — Syncthing's proven pattern, scoped down:** per-file **version vectors** (one entry per machine; the editing machine bumps its counter). Machines exchange deltas over the existing mesh; every machine converges to the global version. No central authority needed — works even relay-free on LAN.
- **Conflicts (rare, but honest):** concurrent edits to the same file → keep both: the loser is saved as `opencode.conflict-<machine>-<ts>.jsonc` and you get a notification with a one-tap diff/merge prompt (chezmoi-style three-way merge later). Never silent last-writer-wins on configs.
- **Secrets stay out:** synced files must not contain API keys — a pre-sync scan flags secret-shaped strings; supported escape hatch is template variables (`${ARREO_ENV:OPENAI_API_KEY}`) resolved per machine from its own keychain. A synced provider list never leaks keys to the RPi in your living room.
- **First-class harness presets:** built-in declarations for opencode, Codex, Pi, [CC], Gemini CLI, Grok — their config paths and safe-sync fields known out of the box; user adds custom files freely.
- **Undo:** every synced file keeps a rolling history in the local SQLite store; `arreo sync revert <file>` restores any previous version. Providers list broken at 3 a.m.? One command.

### 3.9 Harness integrations (deep state, not just heuristics)

> The harness is agnostic (anything CLI runs in a pane), but certain harnesses get **deep integrations** for richer state: what it's doing, whether it has a question, attention-needed notifications — without opening the terminal.

- **The flagship moment — "the agent is asking YOU":** the single most valuable detection is when a harness blocks on a question. Every harness does it (opencode, Pi, OMP, [CC], Codex…), and it's where humans lose hours walking panes. Detection, best-effort by tier:
  1. **Native tier:** the harness *tells us* — permission request / prompt event with the actual text. State becomes `question` with context payload ("May I run `sudo apt install`?") and quick-action buttons.
  2. **Universal tier (works for ANY harness, zero integration):** output-silence + cursor parked at a prompt-shaped last line (per-harness regexes: `?`, `(y/n)`, `❯`, `›`, choice menus) + optional terminal bell. State becomes `question (inferred)` — labeled honestly, never silently pretending certainty.
  3. **Assisted tier:** a tiny optional helper (`arreo agent watch` or a harness hook the user opts into) that calls `arreo agent state <id> question --text "…"` for exact payloads on harnesses without native events.
- **`question` is a first-class state** — distinct from generic `blocked` — and it lights up everywhere at once: TUI sidebar (color + pulsing dot), phone push (with quick-reply), web badge, and `arreo agents --attention` in scripts. A terminal with Arreo open shows, at a glance, which of the 30 agents is waiting on you.
- **Answer from anywhere:** quick-reply sends text into the pane (E2E, audited) — from notification, phone, web, or another agent via the socket API. Harness-agnostic: it's just input to the PTY.
- **Integration tiers:**
  1. **Native events** — harnesses with hooks/event streams ([CC] hooks, Codex events, opencode plugins, Pi events): structured tool-use, permission requests, question states. Richest: we know *what* tool it's running and *what* it's asking.
  2. **ACP (Agent Client Protocol)** — where agents expose it, speak JSON-RPC sessions/prompts/permission-requests for protocol-level state instead of screen scraping.
  3. **OSC + heuristic fallback** — everything else still gets working/blocked/idle/unknown via the universal tier.
- **Notification rules engine:** per-agent rules (blocked > 2 min → push; question → push always; memory cap breach → push + action buttons). Digest mode for the 30-agent fleet: one notification summarizing attention-needing agents instead of 30 pings.
- **State payloads, not just flags:** an integration can attach context to a state — "blocked: awaiting permission `sudo apt install`" with quick-action buttons (allow/deny) straight from the notification. This is what Orca's mobile shows; we make it adapter-driven and community-extendable.
- **Adapter SDK:** TOML registry for detection + optional script hooks (stdin/stdout JSON) for events — community adds harness support without forking Arreo; deep integrations for [CC], Codex, opencode, Pi, Gemini, Grok ship from us.

### 3.10 Web dashboard (manage everything from a browser)

> A paid-benefit surface under our domain: open a URL, log in by pairing, see and drive every machine — no mobile app needed.

- **Transport:** **WebTransport** (HTTP/3) — Baseline across browsers since March 2026 (Safari 26.4 closed the last gap) — talking to the same relay, same protocol semantics as native clients. **WebSocket fallback** for locked-down networks; E2E is at the payload layer so the fallback doesn't weaken the model.
- **E2E in the browser:** the web client is just another device — pairing via the same code/QR ritual, device keypair generated with **WebCrypto**, stored in IndexedDB (non-extractable keys where available). The relay still never sees plaintext.
- **Same Rust core, compiled to WASM** for protocol/crypto/grid logic — the browser client is a third thin UI over the exact same core as mobile (third UI, one logic).
- **Scope:** overview of all machines and agents, states, RAM, notifications, quick answers, approvals, terminal view. Deeper editing stays on TUI/phone — the browser is the "at a random PC" surface.
- **Security nuance:** web sessions are revocable like any device, auto-expiring by default (configurable), and clearly labeled in `arreo devices`.

### 3.11 Cross-platform (hard requirement: Windows, Linux, macOS — all perfect)

> Decided at kickoff: the three desktop OSes are first-class **from Phase 0**, not ported later. This is both a requirement and a differentiator — several competitors are Mac-only.

| Concern | Linux | macOS | Windows |
| --- | --- | --- | --- |
| PTY | `portable-pty` (posix_openpt) | `portable-pty` (posix_openpt) | `portable-pty` (**ConPTY**) — WezTerm runs it in production on Windows daily |
| Daemon autostart | systemd user unit (+ system unit option) | launchd LaunchAgent | Windows Service (`windows-service` crate) + auto-start on login |
| RAM limits per agent | cgroups v2 (`memory.max`, `pids.max`) | advisory (rlimit + monitor + kill) | **Job Objects** (memory + process-count limits, native) |
| Config/data paths | XDG (`dirs` crate) | `~/Library/Application Support` | `%APPDATA%` |
| Graceful shutdown | SIGTERM handling | SIGTERM handling | Service stop / CTRL events |
| TUI backend | ratatui + crossterm | ratatui + crossterm | ratatui + crossterm (Windows Terminal first-class; legacy conhost degraded) |
| Clipboard | OSC 52 / arboard | OSC 52 / arboard | arboard (Win32 clipboard) |

- **Windows reality:** ConPTY needs Win10 1809+; force UTF-8 codepage; Windows Terminal gives full truecolor + mouse; legacy conhost degrades gracefully (quantized colors, limited mouse) via capability detection — never crash, never lie.
- **Anti-"ported-later" rule:** the full functional battery (spawn agent, split, state change, attach/detach, metrics, pairing, update handoff) runs in CI on real Windows/Linux/macOS runners for **every** release tag. A release is not a release if any OS is red.
- **Packaging:** cargo-dist — single binaries, PowerShell one-liner install (like Herdr's), `.msi`, Homebrew; `arreo service install` generates the systemd/launchd/Service unit.

### 3.12 Theming (opencode-style: themes independent of the terminal)

- **Truecolor-first:** themes emit explicit 24-bit RGB SGR sequences, so a theme looks identical in iTerm2, Alacritty, Kitty, Windows Terminal, or over SSH — the terminal's own palette is bypassed. This is exactly how opencode does it and it answers "themes that don't depend on the terminal they open in".
- **Capability detection & fallback:** probe `COLORTERM`/`TERM`; truecolor → RGB; 256-color → nearest quantization; 16-color → nearest ANSI; `NO_COLOR` honored; legacy macOS Terminal.app / conhost take the quantized path (color-compat shim). Never glitched output.
- **`system` theme:** adapts to the terminal (gray scale generated from background luminance + ANSI 0–15 + `none` tokens) — for users who want Arreo to blend with their terminal.
- **JSON theme format (opencode-compatible shape):** `defs` (reusable colors: hex, ANSI index, or references) + `theme` (semantic tokens: `primary`, `text`, `textMuted`, `background`, `backgroundPanel`, `border`, `diff*`, `markdown*`, `syntax*`, …). Every token accepts `{"dark": ..., "light": ...}` variants; `"none"` inherits the terminal default.
- **Hierarchy:** built-in themes (embedded in binary) → `~/.config/arreo/themes/*.json` → `<project>/.arreo/themes/*.json` → `./.arreo/themes/*.json`. Later wins. `/theme` picker in the TUI; `theme` key in config.
- **Built-ins at v1:** `arreo` (our identity), `tokyonight`, `catppuccin`, `gruvbox`, `nord`, `everforest`, `ayu`, `one-dark`, `system`.
- **Default `arreo` look (proposal):** dark-first, near-black background (#0E1116), soft cyan primary (#7DCFFF); **state colors are the product's visual language** — `working` = cyan, `blocked` = amber (#E0AF68), `done` = green (#9ECE6A), `idle` = muted gray, `error` = red (#F7768E). Same hues on TUI and phone; light variants auto-derived. The agent list reads like a status board at a glance from across the room.
- **One theme, every surface:** the same JSON drives TUI **and** mobile — tokens map to SwiftUI `Color` / Compose `ColorScheme` via the shared Rust core. Theme a server, every paired device follows.
- Community theme gallery on the website (cheap OSS-contributor funnel, same as opencode).

### 3.13 Auto-update (install without breaking anyone's work)

> Requirement: updates must never kill agents, drop sessions, or force anyone to save their work.

- **Channels & trust:** `stable` channel; releases built by cargo-dist, signed (minisign/sigstore), verified before staging. `arreo update` or automatic check (configurable; default: check daily, install server-side updates only when safe).
- **Client (TUI/CLI) update — easy path:** download → verify → atomic binary swap → client process restarts and reattaches via resume token. The daemon owns the agents, so a client restart costs seconds and touches nobody else.
- **Server live handoff ("zero-cut update"):**
  1. New binary staged and verified as `arreo-server.next`.
  2. New process starts, connects to the old daemon over the admin socket; protocol-version handshake.
  3. Old daemon transfers: PTY master fds (**Unix: SCM_RIGHTS fd passing**; Windows: inherited ConPTY pseudoconsole handles), scrollback state (disk-backed, so it's a pointer swap, not a copy), SQLite handles closed/reopened, relay session tokens.
  4. Old daemon drains and exits; new daemon serves. Clients reconnect transparently — session IDs live in SQLite, resume tokens stay valid.
  5. **Rollback is trivial:** the previous binary is retained; if the handoff fails at any step, the new process exits and the old daemon continues untouched — never half-dead.
- **Windows fallback:** if ConPTY handle inheritance fails on some OS build, "deferred update": binary swapped atomically, takes effect at next daemon restart (reboot or explicit `arreo server restart`), never forced while agents run. The UI shows "update pending".
- **Protocol compat:** versioned MessagePack schema with an N−1 compatibility window — an old client against a new server (or vice versa) keeps working, so updates can never strand an attached client. Major-break Handoffs are refused and scheduled as deferred updates.
- **Mobile:** store-driven; the Rust core keeps the N−1 window so an outdated phone still pairs.

### 3.14 Relay persistence (offline is normal, 15 days is fine)

> Requirement: power the server off for two weeks; turn it on; everything resumes as if nothing happened.

- **The relay is durable, not stateless:** per-device registry (device certs, last-seen timestamps) + **per-device encrypted inbox** (E2E blobs — the relay stores ciphertext with TTL, default 30 days; it never can read them).
- **While the server is offline:**
  - Phone gets a push: "workbox offline (last seen 2 d ago)"; app shows last cached state + agent states as known-at-disconnect, clearly labeled.
  - Commands/approvals sent from the phone are **queued** in the relay inbox, marked "queued — will deliver on reconnection", never silently dropped; they expire with the TTL.
- **When the server comes back (even 15 days later):**
  - Fresh Noise-KK handshake — device certs are pinned static keys, **nothing expires by age** (revocation is explicit), so the 15-day gap is a non-event at the crypto layer (QUIC 0-RTT/tickets only accelerate same-day resumes; correctness never depends on them).
  - Cursor-based drain of the queued inbox, then snapshot-on-attach per client. Agents' PTY state: preserved by the daemon while powered; restored from SQLite + scrollback if the machine rebooted.
- **Backoff & presence:** reconnect with jittered exponential backoff; relay tracks last-seen per device; `arreo status` and the phone show "last seen X" instead of a scary error.
- Self-hosted relays get the same durable inbox (SQLite/WAL), so the free tier isn't a worse tier.

### 3.15 Plugin system (customize the UI, safely)

> Requirement: anyone can extend Arreo — add UI elements, new pane behaviors, custom widgets — easily, without forking, without endangering stability.

- **Runtime: WASM components (wasmtime + WASI 0.2).** Plugins are sandboxed `.wasm` modules with **declared capabilities** — no ambient authority. A plugin can only do what its manifest grants: `read-agent-state`, `add-widget`, `add-command`, `add-keybinding`, `notify`, `modify-theme-tokens`. Anything else fails at the capability gate, not at 2 a.m. in your render loop.
- **Authoring:** Rust first-class (compile to `wasm32-wasip2`), plus anything that targets the Component Model (Go, JS, Python via community toolchains). A `arreo plugin new` scaffold + a typed host API (the same crate the core uses) makes a "hello widget" a ~30-line project.
- **UI contribution points (v1):**
  - **Sidebar widgets** — small declarative components under/over the agent list (e.g. a token-spend meter, a CI status strip).
  - **Status card extensions** — extra badges/rows on agent cards (provider quota left, task name from Linear).
  - **Custom pane types** — a plugin can register a pane renderer fed by its own logic or an agent's structured output (dashboards, diff viewers, custom visualizations) using a cell/text render API — declarative, so the plugin can never corrupt the render loop or the PTYs.
  - **Commands & keybindings** — `arreo plugin cmd` / TUI keymap entries.
  - **Notification actions** — plugins can add buttons to question/approval notifications.
  - **Theme token extensions** — plugins may define additional tokens (consumed by themes, respecting user theme override).
- **Hot-load:** drop a `.wasm` into `~/.config/arreo/plugins/` → live reload, no daemon restart (component reload over the admin socket). Remove it → gone. No restart, consistent with 3.13.
- **Performance budget:** plugin render calls are declarative and tick-budgeted (e.g. ≤ 4 ms/tick total across plugins); a misbehaving plugin gets throttled and flagged in `arreo plugins doctor`, never hangs the UI.
- **Distribution:** community registry on the website (the Herdr playbook — 1,063 plugins is a moat), signed modules, capability list shown before install (like phone app permissions). Themes (3.12) and adapters (3.9) are already data — plugins are the code-shaped extension point.
- **Mobile/web:** v1 plugins render on TUI only; the declarative widget schema is designed so mobile/web can adopt the same components later (progressive, not blocked).

---

## 4. Security model

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
| Server reattach after 15 days offline | < 3 s to full overview (handshake + cursor sync) |
| Queued messages lost during downtime | 0 within relay retention window |
| Cross-machine attach (VPS → Pi, any client) | < 3 s to live overview of the remote machine |
| Config sync propagation (add provider on 1 machine) | visible on 2nd machine < 10 s when both online; on reconnect otherwise |
| Live server update handoff | agents never die; client reattach < 2 s; rollback = old binary continues |
| Crash safety | PTY scrollback never lost; server crash = restore on restart |

CI runs a nightly benchmark: 30 panes × [CC]-shaped traffic replay, asserting every budget above (this doc becomes `perf-budget.toml`).

---

## 6. Roadmap

> Solo-dev cadence assumed, developed in agent loops (Codex/Grok/[CC] do the heavy lifting). **No calendar dates on purpose:** milestones are defined by exit criteria, not weeks — with AI-accelerated development, phases move at whatever speed the loops sustain, and the exit criteria keep quality honest regardless of pace.

### Phase 0 — Spike (weeks 1–6) *"prove the daemon"*

- Rust skeleton: PTY manager + alacritty_terminal embed + ring buffer; **ConPTY smoke test on Windows first** (if ConPTY + portable-pty can't hold a pane early, we know now, not much later).
- **Agent-friendly foundations (§10):** workspace layout + `AGENTS.md` + `xtask e2e/bench` + `tasks/` files — the repo is developed by agent loops from the very first commit.
- State detection for **[CC] only** (hooks + heuristics).
- `arreo attach` CLI; RAM sampler; 10-agent load test vs budget.
- **Exit demo:** 10 agents, live states, < 100 MB RSS, TUI-less raw CLI — proven on Linux, macOS, and Windows.

### Phase 1 — Core alpha *"self-hosted, local"*

- ratatui TUI (panes, sidebar, mouse); session restore; SQLite.
- **Theming engine (3.12):** truecolor JSON themes, `system` theme, capability fallback, `/theme` picker; 4 built-ins minimum.
- Adapters: [CC], Codex, Pi, opencode, Gemini CLI.
- **Universal `question` detection (3.9)**: silence + prompt-shape + bell for any harness; native payloads for the integrated five.
- Socket API v1 (`read/send/wait/spawn/attach/metrics`) + agent skill doc.
- Private alpha: 20 friendly users (this server's owner first).
- **Exit:** daily-drivable locally; OSS repo public-ready.

### Phase 2 — Remote & security *"the differentiator"*

- Noise/QUIC transport; SPAKE2 pairing; device certs + revocation.
- Relay v0 (self-hostable) **with durable per-device inbox + presence (3.14)**; remote TUI attach; audit log.
- **Auto-update v1 (3.13):** signed releases, client atomic swap, **live server handoff on Unix**; Windows deferred-update fallback; N−1 protocol window.
- **Multi-machine mesh v1 (3.7):** machine directory, `arreo machines`, cross-server attach (VPS ↔ Raspberry Pi flows), per-machine device trust.
- cgroups limits + kill switches + metrics history.
- **Public OSS launch** (Show HN / r/rust). No pricing yet — stars and word of mouth.
- **Exit:** a stranger pairs a second machine in < 5 min without docs help.

### Phase 3 — Mobile beta *"pocket control"*

- Rust core via UniFFI; **iOS (SwiftUI) and Android (Compose) built in parallel** — shared core means duplicated UI only.
- Pairing QR; overview; agent detail; quick answers; push notifications (blocked/done, **including offline-queued delivery**); RAM meters; **themes from the server's JSON** (one theme, every surface).
- **Exit:** the demo from the tagline — 30 agents on a server, managed from a phone (both stores' beta tracks).

### Phase 4 — Depth *"harness fleet + web"*

- Worktree-per-task module; diff review (desktop full, mobile read + comment-back).
- **Harness config sync GA (3.8):** version vectors, conflict copies + merge prompt, secret scanning, revert history, presets for opencode/Codex/Pi/[CC]/Gemini/Grok.
- **Deep harness integrations (3.9):** native events + ACP adapters, notification rules engine, quick-action notifications; community adapter SDK.
- **Web dashboard beta (3.10):** WASM core, WebTransport (+ WS fallback), pairing from browser, all-machines overview under our domain (managed relay users).
- Approval gates; GitHub/Linear task surfaces; plugins SDK v0.
- **Plugin system v1 (3.15):** WASM component runtime, capability manifest, sidebar widgets + status card extensions + commands; hot-load; `arreo plugin new` scaffold.
- **Exit:** the owner adds a provider once and it reaches every machine; a team uses worktrees + mobile approvals as their main loop; a third-party "hello widget" plugin hot-loads without touching the core.

### Phase 5 — Monetize & GA *"the pricing panel"*

- Managed relay (multi-region) **with 30-day durable retention as the default**, billing (Stripe), security audit (external, published), threat model, landing + pricing page. **Update channels + staged rollout via relay** (canary cohorts before fleet-wide). **Community plugin registry** (signed modules, capability display before install).
- Freemium relay GA (see §7): free tier limits enforced, paid tiers unlock web dashboard under our domain, longer retention, unlimited machines.
- Team tier (roles, org machines, SSO later).
- **Exit:** GA + first revenue.

---

## 7. Pricing (open-core; freemium relay — free with limits, paid unlocks everything)

| Tier | Price | What's in it |
| --- | --- | --- |
| **OSS / Self-host** | Free forever | Server + TUI + CLI under **Apache-2.0**; relay under **AGPL-3.0** (decided) — unlimited agents, **self-hosted relay included** with the full feature set (durable inbox, machine directory, push); the whole product if you run it yourself. AGPL keeps third parties from productizing the managed relay without contributing back. |
| **Relay Free (managed)** | Free, no card | The managed relay with **limits**: up to 2 paired machines, inbox retention 7 days, push notifications, mobile sync. Enough to live the whole workflow — not enough to run a fleet. |
| **Cloud Personal** | **$9/mo** | Limits removed: unlimited machines, inbox retention 30 d, metrics history (30 d), device management UI, config sync across machines. |
| **Cloud Pro** | **$19/mo** | + **web dashboard under our domain** (browser client, no app needed), metrics retention (1 y), diff review on mobile, approval gates, priority relay regions, early features. |
| **Team** | **$15/user/mo** (min 5) | + shared machine registry, roles (viewer/operator/admin), audit log export, SSO (SAML/OIDC), centralized device policies, web dashboard for the whole team. |
| **Enterprise** | Custom | On-prem relay license, SLA, security review support, priority adapter requests. |

**The freemium line:** the relay is the network — free users get the network working (2 machines, 7-day retention); paying removes limits and unlocks the web surface under our domain (a managed, auth'd, browser-based control plane we operate). Self-hosters bypass all of it by running the AGPL relay themselves — the classic Tailscale-shaped funnel. **Unit economics:** relay + push ≈ $1–2/user/mo at small scale → ~80% gross margin. Herdr's cloud is "coming soon" — being first to a *credible, audited* paid remote story is the wedge.

---

## 8. Risks & mitigations

| Risk | Mitigation |
| --- | --- |
| Agent CLI churn (21+ CLIs, breaking changes) | Adapter registry as data (TOML), 3-tier integrations (native events → ACP → heuristics) degrade gracefully, community adapter SDK, coverage tests in CI. |
| Config sync corrupts a harness config | Per-file opt-in only; version vectors + conflict copies (never silent LWW); secret-shape scanning blocks key leakage; rolling history + `arreo sync revert` per file. |
| Herdr ships cloud first | We don't out-feature them; we out-*position* (security + resources + tiny mobile). Speed on P2/P3 is existential — protect those phases. |
| iOS background limits | Push-first design; snapshot-on-open; live view only when foregrounded. Design for it, don't fight it. |
| Solo bandwidth | Ruthless MVP cuts (no web dashboard in v1); **both-platforms mobile makes a mobile collaborator a P3 requirement, not an option** — recruit during P2. |
| Windows ConPTY edge cases (resize quirks, legacy conhost, UTF-8) | Smoke test at the very start of Phase 0 (fail fast, not later); Windows Terminal as the supported baseline, conhost degrades gracefully via capability detection; CI matrix on real Windows runners gates every release. |
| Live handoff fails on some platform/build | Handoff is atomic with automatic rollback (old daemon continues untouched); deferred-update fallback never forces a cut while agents run; N−1 protocol window keeps clients unstranded. |
| Security incident / overpromising | External audit before revenue; published threat model; "can't-read-your-traffic relay" is enforceable architecture, not marketing. |
| Rust UI velocity on mobile | UIs are small: 5 screens per platform. The hard logic lives in the shared Rust core, tested off-device; any fix ships once in Rust, twice in thin UI. |

---

## 10. Building Arreo with agents (the repo is developed in agent loops)

> The owner runs OMP/Pi agents in `/loop` with prompts; the fleet implements, tests E2E, hunts bugs, and measures performance — with humans (and native review) as the approval gate. The roadmap has no dates because this is the intended development mode.

### 10.1 Repo is agent-friendly by design (a prerequisite, not an afterthought)

- **Workspace layout for machines:** crates (`arreo-core`, `arreo-server`, `arreo-cli`, `arreo-relay`, `arreo-plugin-api`) with strict dependency direction, so an agent working on `arreo-core` can't accidentally break the TUI. `cargo xtasks` for dev workflows (`xtask e2e`, `xtask bench`, `xtask conpty-smoke`).
- **The spec is the source of truth:** this ROADMAP + `specs/` (per-crate design docs) + `tasks/` (machine-readable task files with acceptance criteria) — agents pick tasks from files, never from chat context.
- **Agent skill doc from day one:** `AGENTS.md` + `arreo dev` skill (Herdr's playbook) teaching any harness how to build, test, and measure Arreo inside a pane — dogfooding our own state detection on our own agents.
- **E2E suite as the referee:** hermetic, fast (< 5 min full), deterministic fixtures (scripted PTY output replays), covering every release-gating behavior on all three OSes. Agents can run it freely; it is the definition of "works".
- **Perf budgets are executable:** `perf-budget.toml` (from §5) enforced by `xtask bench` — a PR that regresses RSS or attach latency fails mechanically, no human judgment needed.

### 10.2 The loop layout (validated by Cursor's autonomous-codebases research, Feb 2026)

> Cursor ran this exact experiment at scale: recursive planners with single-threaded
> accountability, workers that never talk to each other, one deliverable per worker,
> **no integrator**, and a low-but-stable error rate converged by the fleet — ~1,000
> commits/hour sustained for a week with zero human intervention. Their failed experiments
> are as instructive as their wins: lock-based self-coordination collapsed (20 agents at
> 1–3-agent throughput), a separate integrator became a bottleneck bureaucracy, and
> 100%-correct-before-commit serialized everything.

- **Planner-executor (the loop agent):** decomposes roadmap into focused tasks, delegates,
  integrates deliverables, keeps the ledger. One hat at a time — never both.
- **Subagent fleet (OMP ≥ v18 native):** `scout` (exploration, read-only) · `task`
  (implementation workers, recursive to depth 3) · `sonic` (mechanical) · `librarian`
  (external API research) · `reviewer` + `security-reviewer` (independent verification —
  **never the writer grading itself**, which replaces the naive verifier from v1 of this
  design). Workers run on their **own worktrees** (`omp worktree`) and deliver structured
  results (`yield`); the planner integrates sequentially in the ledger.
- **Concurrency discipline:** no shared coordination files, no agent-managed locks
  (validated failure mode) — isolation by worktree, contention resolved by the planner,
  convergence accepted where files collide.
- **Correctness cadence:** full e2e battery + perf budgets green at **integration points**
  (merge to main, phase exit); within worker worktrees, momentum over ceremony. The green
  line is a cadence, not a constant.
- **Prompt discipline (from the same research):** constraints over instructions, concrete
  numbers over vague goals ("generate 20–100 tasks"), never checklist a high-level task,
  and specify intent explicitly (dependencies philosophy, performance limits) — agents
  follow bad instructions as faithfully as good ones.
- **Planning:** a human + one planning agent maintain `tasks/` (acceptance criteria per
  task, dependency order). Nothing enters a loop without written criteria.
- **Bug hunters:** scheduled red-team loops running chaos tests (kill the daemon
  mid-handoff, drop relay connections, OOM a pane, corrupt scrollback files) plus fuzzing
  on the protocol parser and state engine. Findings become tasks with repro steps.
- **Review gate:** native review / dual-review skill before merge; humans approve merges
  and releases, agents do everything else.
- **Empirical verification matrix (every artifact exercised, then attacked):** code →
  failing test + suite + e2e slice; TUI → driven interactively via scripted PTY with real
  key events + frame captures; web → real-browser click-through (CDP/Playwright-style)
  with screenshots per claimed state; mobile → simulator run + screenshots; daemon →
  socket-API exercise + chaos (kill mid-handoff, corrupt input, OOM); docs/themes →
  examples executed. Evidence stored per task under `.loop/evidence/` and referenced from
  the ledger — **claims without evidence don't merge**. After it works, agents run an
  explicit adversarial pass (malformed input, zero-length, huge output, network loss,
  concurrency) — found bugs become tasks or in-scope fixes; dogfooding on our own fleet
  whenever the feature exists.
- **Simplicity is a merge gate, not a taste:** simplest design that meets the criteria;
  every abstraction pays rent (used in ≥ 2 real places); a change must be explainable in
  one sentence; deleted complexity counts as progress. Current stable toolchain always —
  stale dependencies are bugs.
- **Memory:** ledger snapshot + event log (`.loop/PROGRESS.md`), session summaries, and
  `specs/` updates persist learnings between iterations.
- **Fleet infra:** run loops on the VPS (many-core, fast disk); the Pi5 is a *target*
  machine, not a build farm — the research found concurrent-build disk I/O is the first
  bottleneck at fleet scale.

### 10.3 Why this is viable for THIS project specifically

- The product's own feedback loop is unusually machine-checkable: states, metrics, budgets, and protocol conformance are all assertable — ideal ground for agent-driven development.
- The harness for the loops is OMP itself — which already has the subagent machinery the research validated (bundled task agents, model roles, worktrees, structured deliverables). We build Arreo with the same pattern Arreo is meant to serve; every lesson from the loop becomes product insight for Arreo's own state/notification design.
- Rust's compiler + clippy + the e2e battery give agents the fast, unambiguous feedback they need to self-correct in-loop.
- The risks this must NOT degenerate into: agents shipping plausible-but-broken systems code (mitigations: capability-scoped crates, TDD on personal edits, reviewer/security-reviewer independence, CI OS matrix as merge authority), and the loop agent absorbing everything instead of delegating (mitigation: one-hat rule + role table in PROMPT.md).

---

## 9. Decisions log

| Decision | Outcome (2026, kickoff) |
| --- | --- |
| Name | **Arreo** (decided) |
| Mobile stack | **SwiftUI + Jetpack Compose over shared Rust core via UniFFI** (decided) |
| Mobile order | **iOS and Android in parallel from Phase 3** (decided; ~+30% phase effort, collaborator required) |
| License | **Apache-2.0 core + AGPL-3.0 relay** (decided; protects the managed service) |
| Desktop platforms | **Windows + Linux + macOS all first-class from Phase 0** (decided; hard requirement) |
| Theming | **opencode-style JSON, truecolor-first, terminal-independent; same theme JSON across TUI and mobile** (decided) |
| Updates | **Auto-update with live server handoff (agents never cut); deferred-update fallback on Windows; N−1 protocol window** (decided) |
| Multi-machine | **Uniform protocol: servers are clients with PTYs; machine directory at relay; per-machine device trust; VPS ↔ Pi as core scenario** (decided) |
| Config sync | **Opt-in per file, Syncthing-style version vectors, conflict copies + merge prompt, secrets excluded via template variables** (decided) |
| Web dashboard | **Paid surface under our domain; WebTransport first + WS fallback; E2E via WebCrypto; same Rust core as WASM** (decided) |
| Plugins | **WASM components (wasmtime/WASI 0.2), capability-gated manifest, declarative UI contribution points, hot-load, community registry** (decided) |
| Question detection | **`question` is a first-class state; universal tier works for ANY harness via silence + prompt-shape + bell, native tier enriches** (decided) |
| Roadmap dates | **None — phases gate on exit criteria, not calendar weeks** (decided; AI-loop-driven development) |
| Relay persistence | **Durable encrypted inbox, 30-day default retention, explicit revocation only — 15-day offline is a supported flow** (decided) |
| Web dashboard | **Open** — recommendation stands: defer to post-Phase 4, revisit when Team tier demand appears. |

---

*Sources: herdr.dev docs (architecture, socket API, agents, connecting machines, persistence), DeepWiki herdrdev/herdr, github.com/stablyai/orca + onorca.dev docs (mobile companion), opencode.ai/docs/themes (theme system spec), RFC 9382 (SPAKE2), RFC 9001 (QUIC/TLS resumption), magic-wormhole docs, quinn-rs + quinn-hyphae, mozilla/uniffi-rs, bitdrift Rust-SDK binary-size post, alacritty_terminal/portable-pty embed precedents (freya-terminal, teksilo, fresh-editor; WezTerm = ConPTY-in-production precedent), ratatui color-depth docs + color-compat shim patterns, daemon-kit (systemd/launchd/windows-service), tomrochette.com orchestration feature matrix, rustman.org Conductor ecosystem survey.*
